//! Built-in plugins: clean-room versions of Python beancount's, matching
//! their observed behaviour. [`Loader::standard`] registers each under its
//! beancount module name.
//!
//! [`Loader::standard`]: crate::Loader::standard

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::date::Date;
use crate::model::{Entry, EntryKind, ErrorKind, Origin, Source};
use crate::plugin::PluginContext;

/// `beancount.plugins.auto_accounts`: opens every account that no `open`
/// directive names, on the date it is first used.
///
/// Generated opens have no currencies or booking method. Their source is
/// file `<auto_accounts>` with, as "line", the account's position in the
/// sorted list of every account the ledger names, as beancount reports
/// them. An account opened explicitly after its first use is left alone, so
/// the earlier use is still an error. The plugin takes no configuration.
pub fn auto_accounts(entries: &mut Vec<Entry>, ctx: &mut PluginContext<'_>) {
    if ctx.config().is_some() {
        ctx.error(
            ErrorKind::LoadError,
            None,
            "auto_accounts takes no configuration, so it didn't run",
        );
        return;
    }
    let mut first_use: BTreeMap<&str, Date> = BTreeMap::new();
    let mut opened = HashSet::new();
    for entry in entries.iter() {
        if let EntryKind::Open { account, .. } = &entry.kind {
            opened.insert(account.as_str());
        }
        for account in entry.kind.accounts() {
            // Entries are sorted by date, so the first seen is the earliest.
            first_use.entry(account).or_insert(entry.date);
        }
    }
    let generated: Vec<Entry> = first_use
        .iter()
        .enumerate()
        .filter(|(_, (account, _))| !opened.contains(*account))
        .map(|(index, (account, date))| Entry {
            date: *date,
            source: Source {
                file: PathBuf::from("<auto_accounts>"),
                line: index,
            },
            meta: Vec::new(),
            kind: EntryKind::Open {
                account: (*account).to_owned(),
                currencies: None,
                booking: None,
            },
            origin: Origin::Generated(ctx.name().to_owned()),
        })
        .collect();
    entries.extend(generated);
}
