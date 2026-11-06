//! The public data model: what a loaded ledger contains.

use std::path::PathBuf;

use rust_decimal::Decimal;

use crate::date::Date;

/// Where something came from: a file and a 1-based line number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Source {
    /// The file, as resolved when loading.
    pub file: PathBuf,
    /// The 1-based line number.
    pub line: usize,
}

/// A number with a currency or commodity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
    /// The number, with the precision it was written with.
    pub number: Decimal,
    /// The currency or commodity.
    pub currency: String,
}

/// The cost of a lot held at cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cost {
    /// The cost of one unit.
    pub number: Decimal,
    /// The currency the cost is in.
    pub currency: String,
    /// The lot's date: the one given, or else the transaction's.
    pub date: Date,
    /// The lot's label, if it has one.
    pub label: Option<String>,
}

/// A metadata value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaValue {
    /// A string. Accounts, currencies and tags are stored as strings too.
    String(String),
    /// A number.
    Number(Decimal),
    /// A date.
    Date(Date),
    /// `TRUE` or `FALSE`.
    Bool(bool),
    /// A number with a currency.
    Amount(Amount),
    /// A key with no value.
    Null,
}

/// Metadata: keys and values in the order they were written.
pub type Meta = Vec<(String, MetaValue)>;

/// How reductions of lots held at cost are matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Booking {
    /// Reduce only when the lot to reduce is unambiguous.
    #[default]
    Strict,
    /// Reduce the oldest lots first.
    Fifo,
    /// Reduce the newest lots first.
    Lifo,
    /// Don't match lots; allow mixed signs.
    None,
}

impl Booking {
    /// The name as written in a ledger, such as `FIFO`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Booking::Strict => "STRICT",
            Booking::Fifo => "FIFO",
            Booking::Lifo => "LIFO",
            Booking::None => "NONE",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Booking> {
        match name {
            "STRICT" => Some(Booking::Strict),
            "FIFO" => Some(Booking::Fifo),
            "LIFO" => Some(Booking::Lifo),
            "NONE" => Some(Booking::None),
            _ => None,
        }
    }
}

/// The type a `custom` directive's value was written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomType {
    /// A quoted string.
    String,
    /// An account name.
    Account,
    /// A date.
    Date,
    /// A number with a currency.
    Amount,
    /// `TRUE` or `FALSE`.
    Bool,
    /// A number.
    Number,
}

/// One value of a `custom` directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomValue {
    /// What the value was written as.
    pub dtype: CustomType,
    /// The value.
    pub value: MetaValue,
}

/// A posting: one leg of a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    /// The account.
    pub account: String,
    /// The units. Always present after loading, unless the transaction
    /// failed to book.
    pub units: Option<Amount>,
    /// The lot's cost, for postings held at cost.
    pub cost: Option<Cost>,
    /// The price per unit.
    pub price: Option<Amount>,
    /// The posting's flag, such as `!`.
    pub flag: Option<char>,
    /// The posting's metadata.
    pub meta: Meta,
}

/// A transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// The flag: `*` for complete, `!` for incomplete, `P` for padding.
    pub flag: char,
    /// The payee, if one was given.
    pub payee: Option<String>,
    /// The narration, which may be empty.
    pub narration: String,
    /// Tags, sorted and without duplicates.
    pub tags: Vec<String>,
    /// Links, sorted and without duplicates.
    pub links: Vec<String>,
    /// The postings, after interpolation and booking.
    pub postings: Vec<Posting>,
}

/// What kind of directive an entry is, with its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// Opens an account.
    Open {
        /// The account.
        account: String,
        /// The currencies allowed, if constrained.
        currencies: Option<Vec<String>>,
        /// The booking method, if given.
        booking: Option<Booking>,
    },
    /// Closes an account.
    Close {
        /// The account.
        account: String,
    },
    /// Declares a commodity.
    Commodity {
        /// The commodity.
        currency: String,
    },
    /// Asserts an account's balance at the start of a day.
    Balance {
        /// The account; its sub-accounts count too.
        account: String,
        /// The expected balance.
        amount: Amount,
        /// The tolerance, if given explicitly.
        tolerance: Option<Decimal>,
    },
    /// Pads an account from another so the next balance assertion holds.
    Pad {
        /// The account to pad.
        account: String,
        /// The account the padding comes from.
        source_account: String,
    },
    /// The price of a commodity.
    Price {
        /// The commodity priced.
        currency: String,
        /// Its price.
        amount: Amount,
    },
    /// A custom directive.
    Custom {
        /// The type string.
        type_name: String,
        /// The values after the type string.
        values: Vec<CustomValue>,
    },
    /// A transaction.
    Transaction(Transaction),
}

impl EntryKind {
    /// The directive's name as beancount reports it, such as `Transaction`.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            EntryKind::Open { .. } => "Open",
            EntryKind::Close { .. } => "Close",
            EntryKind::Commodity { .. } => "Commodity",
            EntryKind::Balance { .. } => "Balance",
            EntryKind::Pad { .. } => "Pad",
            EntryKind::Price { .. } => "Price",
            EntryKind::Custom { .. } => "Custom",
            EntryKind::Transaction(_) => "Transaction",
        }
    }
}

impl EntryKind {
    /// The accounts the directive names, in the order written: the account
    /// of an `open`, `close` or `balance`, both accounts of a `pad`, and each
    /// posting's account. Other directives name none.
    #[must_use]
    pub fn accounts(&self) -> Vec<&str> {
        match self {
            EntryKind::Open { account, .. }
            | EntryKind::Close { account }
            | EntryKind::Balance { account, .. } => vec![account],
            EntryKind::Pad {
                account,
                source_account,
            } => vec![account, source_account],
            EntryKind::Transaction(txn) => {
                txn.postings.iter().map(|p| p.account.as_str()).collect()
            }
            EntryKind::Commodity { .. } | EntryKind::Price { .. } | EntryKind::Custom { .. } => {
                Vec::new()
            }
        }
    }
}

/// Whether an entry was written in a ledger file or made while loading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A directive in a file.
    Written,
    /// Made while loading, by what the name says: `pad` for padding
    /// transactions, or a plugin's registered name. Never write these back to
    /// a file.
    Generated(String),
}

/// A dated directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The directive's date.
    pub date: Date,
    /// Where it was written, or for a generated entry, what beancount
    /// reports for it (such as file `<auto_accounts>`).
    pub source: Source,
    /// Its metadata.
    pub meta: Meta,
    /// What it is.
    pub kind: EntryKind,
    /// Whether it was written or generated.
    pub origin: Origin,
}

/// The kinds of error, named as Python beancount names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Input that isn't a valid token.
    LexerError,
    /// Valid tokens in an invalid order.
    ParserSyntaxError,
    /// A valid directive with an invalid value, such as an unknown option.
    ParserError,
    /// A file that couldn't be read or included.
    LoadError,
    /// An option that still works but is deprecated.
    DeprecatedError,
    /// A failed check on the ledger's structure.
    ValidationError,
    /// A failed balance assertion.
    BalanceError,
    /// A pad that was never used.
    PadError,
    /// A posting whose amount couldn't be worked out.
    InterpolationError,
    /// More than one posting with no amount.
    CategorizationError,
    /// A reduction that doesn't match lots unambiguously.
    AmbiguousMatchError,
    /// Syntax outside the subset turtlebean supports.
    Unsupported,
}

impl ErrorKind {
    /// The kind's name, such as `BalanceError`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::LexerError => "LexerError",
            ErrorKind::ParserSyntaxError => "ParserSyntaxError",
            ErrorKind::ParserError => "ParserError",
            ErrorKind::LoadError => "LoadError",
            ErrorKind::DeprecatedError => "DeprecatedError",
            ErrorKind::ValidationError => "ValidationError",
            ErrorKind::BalanceError => "BalanceError",
            ErrorKind::PadError => "PadError",
            ErrorKind::InterpolationError => "InterpolationError",
            ErrorKind::CategorizationError => "CategorizationError",
            ErrorKind::AmbiguousMatchError => "AmbiguousMatchError",
            ErrorKind::Unsupported => "Unsupported",
        }
    }
}

/// An error found while loading a ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// What went wrong, broadly.
    pub kind: ErrorKind,
    /// Where, if it can be pinned down.
    pub source: Option<Source>,
    /// A description for people.
    pub message: String,
}

/// Options set with `option` directives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    /// The ledger's title.
    pub title: Option<String>,
    /// Currencies to report in.
    pub operating_currencies: Vec<String>,
    /// Default tolerances by currency, from `inferred_tolerance_default`.
    /// The key `*` applies to every other currency.
    pub tolerance_defaults: Vec<(String, Decimal)>,
    /// Multiplier for inferred tolerances (default 0.5).
    pub tolerance_multiplier: Decimal,
    /// The default booking method.
    pub booking: Booking,
    /// `plugin` directives in the order declared: module name and optional
    /// configuration. Only plugins registered with the [`Loader`] run.
    ///
    /// [`Loader`]: crate::Loader
    pub plugins: Vec<(String, Option<String>)>,
}

/// A loaded ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ledger {
    /// Entries, sorted by date, after booking and padding.
    pub entries: Vec<Entry>,
    /// Errors from every stage.
    pub errors: Vec<Error>,
    /// Options.
    pub options: Options,
}
