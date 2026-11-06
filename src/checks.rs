//! Runs over booked entries in order: inserts padding, checks balance
//! assertions and validates the ledger's structure.

use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;

use crate::booking::default_tolerance;
use crate::date::Date;
use crate::model::{
    Amount, Entry, EntryKind, Error, ErrorKind, Options, Posting, Source, Transaction,
};
use crate::number;

fn error(kind: ErrorKind, source: &Source, message: impl Into<String>) -> Error {
    Error {
        kind,
        source: Some(source.clone()),
        message: message.into(),
    }
}

/// Units held, by account then currency.
type Balances = HashMap<String, HashMap<String, Decimal>>;

fn add_units(balances: &mut Balances, account: &str, amount: &Amount) {
    let entry = balances
        .entry(account.to_owned())
        .or_default()
        .entry(amount.currency.clone())
        .or_default();
    *entry = number::add(*entry, amount.number).unwrap_or(*entry);
}

/// The balance of an account and its sub-accounts in one currency.
fn balance_of(balances: &Balances, account: &str, currency: &str) -> Decimal {
    let prefix = format!("{account}:");
    balances
        .iter()
        .filter(|(a, _)| *a == account || a.starts_with(&prefix))
        .filter_map(|(_, currencies)| currencies.get(currency))
        .fold(Decimal::ZERO, |total, n| {
            number::add(total, *n).unwrap_or(total)
        })
}

struct ActivePad {
    index: usize,
    used: HashSet<String>,
    inserted: bool,
}

/// Inserts padding transactions and checks balance assertions.
pub(crate) fn pad_and_balance(
    entries: Vec<Entry>,
    options: &Options,
    errors: &mut Vec<Error>,
) -> Vec<Entry> {
    // Accounts with an open or a close; balance assertions on any other
    // account are errors, even if it has postings
    let declared: HashSet<String> = entries
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::Open { account, .. } | EntryKind::Close { account } => Some(account.clone()),
            _ => None,
        })
        .collect();

    let mut balances = Balances::new();
    let mut pads: HashMap<String, ActivePad> = HashMap::new();
    let mut inserted: HashMap<usize, Vec<Entry>> = HashMap::new();
    let mut unused: Vec<Source> = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        match &entry.kind {
            EntryKind::Transaction(txn) => {
                for posting in &txn.postings {
                    if let Some(units) = &posting.units {
                        add_units(&mut balances, &posting.account, units);
                    }
                }
            }
            EntryKind::Pad { account, .. } => {
                if let Some(old) = pads.insert(
                    account.clone(),
                    ActivePad {
                        index,
                        used: HashSet::new(),
                        inserted: false,
                    },
                ) && !old.inserted
                {
                    unused.push(entries[old.index].source.clone());
                }
            }
            EntryKind::Balance {
                account,
                amount,
                tolerance,
            } => {
                let mut actual = balance_of(&balances, account, &amount.currency);
                if let Some(pad) = pads.get_mut(account)
                    && pad.used.insert(amount.currency.clone())
                {
                    let difference = number::sub(amount.number, actual).unwrap_or_default();
                    if !difference.is_zero() {
                        let pad_entry = &entries[pad.index];
                        let padding = padding(pad_entry, amount, difference);
                        if let EntryKind::Transaction(txn) = &padding.kind {
                            for posting in &txn.postings {
                                if let Some(units) = &posting.units {
                                    add_units(&mut balances, &posting.account, units);
                                }
                            }
                        }
                        inserted.entry(pad.index).or_default().push(padding);
                        pad.inserted = true;
                        actual = balance_of(&balances, account, &amount.currency);
                    }
                }
                let allowed = tolerance.unwrap_or_else(|| balance_tolerance(amount, options));
                let difference = number::sub(actual, amount.number).unwrap_or_default();
                if !declared.contains(account) {
                    errors.push(error(
                        ErrorKind::BalanceError,
                        &entry.source,
                        format!("unknown account {account}"),
                    ));
                }
                if difference.abs() > allowed {
                    errors.push(error(
                        ErrorKind::BalanceError,
                        &entry.source,
                        format!(
                            "balance of {account} is {actual} {}, expected {} {} (off by {difference})",
                            amount.currency, amount.number, amount.currency
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    for pad in pads.into_values() {
        if !pad.inserted {
            unused.push(entries[pad.index].source.clone());
        }
    }
    for source in unused {
        errors.push(error(ErrorKind::PadError, &source, "unused pad"));
    }

    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        out.push(entry);
        if let Some(padding) = inserted.remove(&index) {
            out.extend(padding);
        }
    }
    out
}

fn padding(pad: &Entry, amount: &Amount, difference: Decimal) -> Entry {
    let EntryKind::Pad {
        account,
        source_account,
    } = &pad.kind
    else {
        unreachable!("not a pad")
    };
    let posting = |account: &str, number: Decimal| Posting {
        account: account.to_owned(),
        units: Some(Amount {
            number,
            currency: amount.currency.clone(),
        }),
        cost: None,
        price: None,
        flag: None,
        meta: Vec::new(),
    };
    Entry {
        date: pad.date,
        source: pad.source.clone(),
        meta: Vec::new(),
        kind: EntryKind::Transaction(Transaction {
            flag: 'P',
            payee: None,
            narration: format!(
                "(Padding inserted for Balance of {} {} for difference {} {})",
                amount.number, amount.currency, difference, amount.currency
            ),
            tags: Vec::new(),
            links: Vec::new(),
            postings: vec![
                posting(account, difference),
                posting(source_account, -difference),
            ],
        }),
    }
}

/// A balance assertion's tolerance: a unit in the last place of the expected
/// amount, scaled by the multiplier (twice the inferred transaction tolerance).
fn balance_tolerance(amount: &Amount, options: &Options) -> Decimal {
    let places = number::decimal_places(amount.number);
    if places == 0 {
        return default_tolerance(&amount.currency, options);
    }
    number::unit(places) * options.tolerance_multiplier * Decimal::TWO
}

struct Opened {
    date: Date,
    currencies: Option<Vec<String>>,
}

/// Checks the ledger's structure: accounts opened before use and not used
/// after closing, currency constraints, and duplicate directives.
pub(crate) fn validate(entries: &[Entry], errors: &mut Vec<Error>) {
    let mut opens: HashMap<&str, Opened> = HashMap::new();
    for entry in entries {
        if let EntryKind::Open {
            account,
            currencies,
            ..
        } = &entry.kind
        {
            opens.entry(account).or_insert(Opened {
                date: entry.date,
                currencies: currencies.clone(),
            });
        }
    }
    let mut seen_opens: HashSet<&str> = HashSet::new();
    let mut closes: HashMap<&str, Date> = HashMap::new();
    let mut balances: HashMap<(&str, Date, &str), Decimal> = HashMap::new();
    let mut commodities: HashSet<&str> = HashSet::new();

    for entry in entries {
        let source = &entry.source;
        let unknown = |account: &str| format!("invalid reference to unknown account {account}");
        match &entry.kind {
            EntryKind::Open { account, .. } => {
                if !seen_opens.insert(account) {
                    errors.push(error(
                        ErrorKind::ValidationError,
                        source,
                        format!("duplicate open of {account}"),
                    ));
                }
            }
            EntryKind::Close { account } => {
                if !opens.contains_key(account.as_str()) {
                    errors.push(error(
                        ErrorKind::ValidationError,
                        source,
                        format!("close of unopened {account}"),
                    ));
                } else if closes.insert(account, entry.date).is_some() {
                    errors.push(error(
                        ErrorKind::ValidationError,
                        source,
                        format!("duplicate close of {account}"),
                    ));
                }
            }
            EntryKind::Commodity { currency } => {
                if !commodities.insert(currency) {
                    errors.push(error(
                        ErrorKind::ValidationError,
                        source,
                        format!("duplicate commodity {currency}"),
                    ));
                }
            }
            EntryKind::Balance {
                account, amount, ..
            } => {
                if !opens.contains_key(account.as_str()) {
                    errors.push(error(ErrorKind::ValidationError, source, unknown(account)));
                }
                let key = (account.as_str(), entry.date, amount.currency.as_str());
                match balances.get(&key) {
                    Some(first) if *first != amount.number => {
                        errors.push(error(
                            ErrorKind::ValidationError,
                            source,
                            "duplicate balance assertion",
                        ));
                    }
                    Some(_) => {}
                    None => {
                        balances.insert(key, amount.number);
                    }
                }
            }
            EntryKind::Pad {
                account,
                source_account,
            } => {
                for account in [account, source_account] {
                    if !opens.contains_key(account.as_str()) {
                        errors.push(error(ErrorKind::ValidationError, source, unknown(account)));
                    }
                }
            }
            EntryKind::Transaction(txn) => check_postings(entry, txn, &opens, &closes, errors),
            EntryKind::Price { .. } | EntryKind::Custom { .. } => {}
        }
    }
}

/// Checks a transaction's postings against when their accounts are open and
/// which currencies they allow. Each account is checked once per transaction.
fn check_postings(
    entry: &Entry,
    txn: &Transaction,
    opens: &HashMap<&str, Opened>,
    closes: &HashMap<&str, Date>,
    errors: &mut Vec<Error>,
) {
    let mut invalid =
        |message: String| errors.push(error(ErrorKind::ValidationError, &entry.source, message));
    let mut checked: HashSet<&str> = HashSet::new();
    for posting in &txn.postings {
        let account = posting.account.as_str();
        let first = checked.insert(account);
        let Some(open) = opens.get(account) else {
            if first {
                invalid(format!("invalid reference to unknown account {account}"));
            }
            continue;
        };
        let after_close = closes.get(account).is_some_and(|close| entry.date > *close);
        if first && (entry.date < open.date || after_close) {
            invalid(format!("{account} is inactive"));
        }
        if let (Some(allowed), Some(units)) = (&open.currencies, &posting.units)
            && !allowed.contains(&units.currency)
        {
            invalid(format!("{} not allowed in {account}", units.currency));
        }
    }
}
