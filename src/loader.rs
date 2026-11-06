//! Loads a ledger from disk: follows includes, applies options, sorts
//! entries, books them, runs plugins, then checks them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;

use crate::booking::{self, Context, Inventory, Outcome};
use crate::checks;
use crate::model::{Booking, Entry, EntryKind, Error, ErrorKind, Ledger, Options, Origin, Source};
use crate::parser::{Parsed, RawEntry, RawKind, RawTransaction, parse_with_replacements};
use crate::plugin::{Plugin, PluginContext};
use crate::plugins;

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

/// Loads a ledger file and everything it includes, running the built-in
/// plugins it declares, as Python beancount's `load_file` does. The same as
/// `Loader::standard().load(path)`.
#[must_use]
pub fn load_file(path: &Path) -> Ledger {
    Loader::standard().load(path)
}

type Registered = Vec<(String, Box<dyn Plugin>)>;

/// Loads ledgers with a chosen set of plugins.
///
/// ```
/// # fn check(_: &mut Vec<turtlebean::Entry>, _: &mut turtlebean::PluginContext<'_>) {}
/// let loader = turtlebean::Loader::new()
///     .plugin("beancount.plugins.auto_accounts", turtlebean::plugins::auto_accounts)
///     .always("myapp.check", check);
/// let ledger = loader.load(std::path::Path::new("main.beancount"));
/// ```
#[derive(Default)]
pub struct Loader {
    declared: Registered,
    always: Registered,
}

impl Loader {
    /// A loader with no plugins. A ledger that declares any gets an
    /// [`ErrorKind::Unsupported`] error for each.
    #[must_use]
    pub fn new() -> Loader {
        Loader::default()
    }

    /// A loader with every built-in plugin, under beancount's module names.
    #[must_use]
    pub fn standard() -> Loader {
        Loader::new().plugin("beancount.plugins.auto_accounts", plugins::auto_accounts)
    }

    /// Registers a plugin to run when a ledger declares it with
    /// `plugin "name"`: in the order declared, once per declaration, with
    /// that directive's configuration. A later registration under the same
    /// name replaces an earlier one.
    #[must_use]
    pub fn plugin(mut self, name: impl Into<String>, plugin: impl Plugin + 'static) -> Loader {
        let name = name.into();
        self.declared.retain(|(n, _)| *n != name);
        self.declared.push((name, Box::new(plugin)));
        self
    }

    /// Adds a plugin that runs on every ledger, whatever it declares, after
    /// the declared ones and in the order added.
    #[must_use]
    pub fn always(mut self, name: impl Into<String>, plugin: impl Plugin + 'static) -> Loader {
        self.always.push((name.into(), Box::new(plugin)));
        self
    }

    /// Loads a ledger file and everything it includes, then books it, runs
    /// the plugins and checks it.
    #[must_use]
    pub fn load(&self, path: &Path) -> Ledger {
        load(path, self)
    }
}

fn load(path: &Path, plugins: &Loader) -> Ledger {
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
    finish(loaded, plugins)
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

/// [`type_order`] for booked entries.
fn kind_order(kind: &EntryKind) -> i8 {
    match kind {
        EntryKind::Open { .. } => -2,
        EntryKind::Balance { .. } => -1,
        EntryKind::Close { .. } => 2,
        _ => 0,
    }
}

/// Runs the declared plugins that are registered, then the ones that always
/// run, and sorts the result as the entries were sorted before booking.
fn run_plugins(
    mut entries: Vec<Entry>,
    options: &Options,
    registered: &Loader,
    errors: &mut Vec<Error>,
) -> Vec<Entry> {
    let declared = options.plugins.iter().map(|(name, config)| {
        let plugin = registered.declared.iter().find(|(n, _)| n == name);
        (name.as_str(), config.as_deref(), plugin.map(|(_, p)| p))
    });
    let always = registered
        .always
        .iter()
        .map(|(name, p)| (name.as_str(), None, Some(p)));
    let mut ran = false;
    for (name, config, plugin) in declared.chain(always) {
        let Some(plugin) = plugin else {
            errors.push(Error {
                kind: ErrorKind::Unsupported,
                source: None,
                message: format!(
                    "turtlebean doesn't run plugin \"{name}\", so its changes are missing"
                ),
            });
            continue;
        };
        let mut ctx = PluginContext {
            name,
            config,
            options,
            errors,
        };
        plugin.run(&mut entries, &mut ctx);
        ran = true;
    }
    if ran {
        // Stable, so entries that tie keep their order.
        entries.sort_by_key(|e| (e.date, kind_order(&e.kind), e.source.line));
    }
    entries
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

fn finish(loaded: Loaded, plugins: &Loader) -> Ledger {
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
            origin: Origin::Written,
        });
    }

    let booked = run_plugins(booked, &options, plugins, &mut errors);
    let entries = checks::pad_and_balance(booked, &options, &mut errors);
    checks::validate(&entries, &mut errors);
    Ledger {
        entries,
        errors,
        options,
    }
}
