//! A clean-room parser and lossless writer for a subset of the
//! [beancount](https://beancount.github.io/docs/) plain-text accounting
//! format.
//!
//! - [`load_file`] reads a ledger and everything it includes, books it and
//!   checks it, aiming for the same results and errors as Python beancount.
//! - [`Loader`] does the same with a chosen set of [`Plugin`]s.
//! - [`parse`] and [`write`] turn text into a [`Document`] and back, byte for
//!   byte.
//!
//! Syntax outside the supported subset (see the README) is reported as an
//! [`ErrorKind::Unsupported`] error rather than guessed at.

mod booking;
mod checks;
mod date;
mod lexer;
mod loader;
mod model;
mod number;
mod parser;
mod plugin;

pub use date::Date;
pub use loader::{Loader, load_file};
pub use model::{
    Amount, Booking, Cost, CustomType, CustomValue, Entry, EntryKind, Error, ErrorKind, Ledger,
    Meta, MetaValue, Options, Origin, Posting, Source, Transaction,
};
pub use parser::{Document, parse, write};
pub use plugin::{Plugin, PluginContext};
pub use rust_decimal::{Decimal, RoundingStrategy};
