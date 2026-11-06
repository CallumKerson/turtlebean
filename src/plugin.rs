//! The plugin interface: functions that change a ledger's entries while it
//! loads, after booking and before the balance checks and validation.

use crate::model::{Entry, Error, ErrorKind, Options, Source};

/// Something that changes or checks a ledger's entries while it loads. Any
/// `Fn(&mut Vec<Entry>, &mut PluginContext<'_>)` that is `Send + Sync` is
/// one, so a plain function or closure works.
///
/// A plugin may add, remove or change entries; the loader sorts them again
/// afterwards. Entries it adds should have [`Origin::Generated`] with
/// [`PluginContext::name`].
///
/// [`Origin::Generated`]: crate::Origin::Generated
pub trait Plugin: Send + Sync {
    /// Runs the plugin over the booked entries, sorted by date.
    fn run(&self, entries: &mut Vec<Entry>, ctx: &mut PluginContext<'_>);
}

impl<F> Plugin for F
where
    F: Fn(&mut Vec<Entry>, &mut PluginContext<'_>) + Send + Sync,
{
    fn run(&self, entries: &mut Vec<Entry>, ctx: &mut PluginContext<'_>) {
        self(entries, ctx);
    }
}

/// What a running plugin can see and report.
pub struct PluginContext<'a> {
    pub(crate) name: &'a str,
    pub(crate) config: Option<&'a str>,
    pub(crate) options: &'a Options,
    pub(crate) errors: &'a mut Vec<Error>,
}

impl PluginContext<'_> {
    /// The name the plugin was registered under, such as
    /// `beancount.plugins.auto_accounts`.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// The configuration string from the `plugin` directive, if any. Always
    /// `None` for plugins added with [`Loader::always`].
    ///
    /// [`Loader::always`]: crate::Loader::always
    #[must_use]
    pub fn config(&self) -> Option<&str> {
        self.config
    }

    /// The ledger's options.
    #[must_use]
    pub fn options(&self) -> &Options {
        self.options
    }

    /// Reports an error, which ends up in [`Ledger::errors`].
    ///
    /// [`Ledger::errors`]: crate::Ledger::errors
    pub fn error(&mut self, kind: ErrorKind, source: Option<Source>, message: impl Into<String>) {
        self.errors.push(Error {
            kind,
            source,
            message: message.into(),
        });
    }
}
