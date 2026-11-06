//! Tests for the plugin API: which plugins run, when, and with what.

use std::path::PathBuf;
use std::sync::Mutex;

use turtlebean::{Entry, EntryKind, ErrorKind, Ledger, Loader, Origin, PluginContext};

/// Writes `text` to a fresh file and loads it with `loader`.
fn load(name: &str, text: &str, loader: &Loader) -> Ledger {
    let dir = std::env::temp_dir().join(format!("turtlebean-loader-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("main.beancount");
    std::fs::write(&path, text).unwrap();
    let ledger = loader.load(&path);
    std::fs::remove_dir_all(&dir).unwrap();
    ledger
}

const LEDGER: &str = "\
plugin \"test.first\" \"one\"
plugin \"test.second\"
plugin \"test.first\" \"two\"

2025-01-01 open Assets:Bank
2025-01-01 open Equity:Opening

2025-01-02 * \"Opening\"
  Assets:Bank  10 GBP
  Equity:Opening
";

#[test]
fn declared_plugins_run_in_order_with_their_config() {
    let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
    let record = |label: &'static str| {
        let calls = std::sync::Arc::clone(&calls);
        move |_: &mut Vec<Entry>, ctx: &mut PluginContext<'_>| {
            calls
                .lock()
                .unwrap()
                .push(format!("{label} {} {:?}", ctx.name(), ctx.config()));
        }
    };
    let loader = Loader::new()
        .always("test.always", record("always"))
        .plugin("test.second", record("second"))
        .plugin("test.first", record("first"))
        .plugin("test.unused", record("unused"));
    let ledger = load("order", LEDGER, &loader);
    assert_eq!(ledger.errors, []);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "first test.first Some(\"one\")",
            "second test.second None",
            "first test.first Some(\"two\")",
            "always test.always None",
        ]
    );
}

#[test]
fn a_later_registration_replaces_an_earlier_one() {
    let loader = Loader::new()
        .plugin(
            "test.first",
            |_: &mut Vec<Entry>, _: &mut PluginContext<'_>| panic!("replaced, so never runs"),
        )
        .plugin(
            "test.first",
            |_: &mut Vec<Entry>, _: &mut PluginContext<'_>| {},
        )
        .plugin(
            "test.second",
            |_: &mut Vec<Entry>, _: &mut PluginContext<'_>| {},
        );
    assert_eq!(load("replace", LEDGER, &loader).errors, []);
}

#[test]
fn unregistered_plugins_are_reported_once_per_declaration() {
    let ledger = load("unregistered", LEDGER, &Loader::new());
    let messages: Vec<_> = ledger
        .errors
        .iter()
        .map(|e| (e.kind, e.source.clone(), e.message.as_str()))
        .collect();
    assert_eq!(
        messages,
        [
            (
                ErrorKind::Unsupported,
                None,
                "turtlebean doesn't run plugin \"test.first\", so its changes are missing"
            ),
            (
                ErrorKind::Unsupported,
                None,
                "turtlebean doesn't run plugin \"test.second\", so its changes are missing"
            ),
            (
                ErrorKind::Unsupported,
                None,
                "turtlebean doesn't run plugin \"test.first\", so its changes are missing"
            ),
        ]
    );
    // The rest of the ledger still loads.
    assert_eq!(ledger.entries.len(), 3);
}

#[test]
fn added_entries_are_sorted_and_checked() {
    // Adds an open dated before everything else, at the end of the list, and
    // a balance assertion that fails.
    let add = |entries: &mut Vec<Entry>, ctx: &mut PluginContext<'_>| {
        let mut open = entries[0].clone();
        open.date = turtlebean::Date::new(2024, 12, 31).unwrap();
        open.kind = EntryKind::Open {
            account: "Assets:Cash".to_owned(),
            currencies: None,
            booking: None,
        };
        open.origin = Origin::Generated(ctx.name().to_owned());
        entries.push(open);
        ctx.error(ErrorKind::ValidationError, None, "a plugin's own error");
    };
    let text = "2025-01-01 open Assets:Bank\n";
    let ledger = load("sorted", text, &Loader::new().always("test.add", add));
    let accounts: Vec<_> = ledger
        .entries
        .iter()
        .flat_map(|e| e.kind.accounts())
        .collect();
    assert_eq!(accounts, ["Assets:Cash", "Assets:Bank"]);
    assert_eq!(
        ledger.entries[0].origin,
        Origin::Generated("test.add".to_owned())
    );
    assert_eq!(ledger.entries[1].origin, Origin::Written);
    assert_eq!(ledger.errors.len(), 1);
    assert_eq!(ledger.errors[0].message, "a plugin's own error");
}

#[test]
fn padding_is_marked_as_generated() {
    let text = "\
2025-01-01 open Assets:Bank
2025-01-01 open Equity:Opening
2025-01-01 pad Assets:Bank Equity:Opening
2025-01-02 balance Assets:Bank  5 GBP
";
    let ledger = load("pad", text, &Loader::new());
    let padding: Vec<_> = ledger
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Transaction(_)))
        .collect();
    assert_eq!(padding.len(), 1);
    assert_eq!(padding[0].origin, Origin::Generated("pad".to_owned()));
}

#[test]
fn accounts_lists_what_a_directive_names() {
    let text = "\
2025-01-01 open Assets:Bank
2025-01-01 pad Assets:Bank Equity:Opening
2025-01-01 price GBP 1.2 USD
";
    let ledger = load("accounts", text, &Loader::new());
    let named: Vec<_> = ledger.entries.iter().map(|e| e.kind.accounts()).collect();
    assert!(named.contains(&vec!["Assets:Bank"]));
    assert!(named.contains(&vec!["Assets:Bank", "Equity:Opening"]));
    assert!(named.contains(&vec![]));
}
