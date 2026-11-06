//! Turns parsed transactions into booked ones: infers missing currencies,
//! matches reductions against lots held at cost, fills in elided amounts and
//! checks that each transaction balances.

use std::collections::HashMap;

use rust_decimal::{Decimal, RoundingStrategy};

use crate::date::Date;
use crate::model::{Amount, Booking, Cost, ErrorKind, Options, Posting, Transaction};
use crate::number;
use crate::parser::{CostSpec, RawPosting, RawTransaction};

/// A lot held at cost in one account.
#[derive(Debug, Clone)]
struct Lot {
    units: Decimal,
    currency: String,
    cost: Cost,
}

/// Lots held at cost, by account.
#[derive(Debug, Default)]
pub(crate) struct Inventory {
    lots: HashMap<String, Vec<Lot>>,
}

#[derive(Debug)]
pub(crate) struct BookingError {
    pub kind: ErrorKind,
    /// The posting's line, or `None` for the transaction's.
    pub line: Option<usize>,
    pub message: String,
}

pub(crate) enum Outcome {
    /// Booked, with an optional error about balancing.
    Booked(Transaction, Option<BookingError>),
    /// Kept without postings because booking failed.
    Failed(Transaction, BookingError),
    /// Dropped altogether.
    Dropped(BookingError),
}

/// Context for booking one transaction.
pub(crate) struct Context<'a> {
    pub date: Date,
    pub options: &'a Options,
    pub methods: &'a HashMap<String, Booking>,
}

fn error(kind: ErrorKind, line: Option<usize>, message: impl Into<String>) -> BookingError {
    BookingError {
        kind,
        line,
        message: message.into(),
    }
}

/// A posting part-way through booking.
#[derive(Debug, Clone)]
struct Work {
    account: String,
    number: Option<Decimal>,
    currency: Option<String>,
    cost: Option<Cost>,
    price: Option<Amount>,
    flag: Option<char>,
    meta: crate::model::Meta,
    line: usize,
    /// Lot reductions to apply if the transaction books: (lot index, units).
    reduces: Option<(usize, Decimal)>,
    /// The transaction failed to book because of this posting.
    auto: bool,
}

pub(crate) fn book(raw: RawTransaction, ctx: &Context<'_>, inventory: &mut Inventory) -> Outcome {
    let RawTransaction {
        flag,
        payee,
        narration,
        tags,
        links,
        postings,
    } = raw;
    let header = Transaction {
        flag,
        payee,
        narration,
        tags,
        links,
        postings: Vec::new(),
    };
    let postings = infer_currencies(postings);
    let booked = match book_lots(&postings, ctx, inventory) {
        Ok(booked) => booked,
        Err(e) => return Outcome::Failed(header, e),
    };
    let interpolated = match interpolate(booked, ctx.options) {
        Ok(p) => p,
        Err(Interpolation::Drop(e)) => return Outcome::Dropped(e),
        Err(Interpolation::Fail(e)) => return Outcome::Failed(header, e),
    };
    let balance_error = check_balanced(&interpolated, ctx.options);
    apply_lots(&interpolated, inventory);
    let txn = Transaction {
        postings: interpolated
            .into_iter()
            .map(|w| Posting {
                account: w.account,
                units: match (w.number, w.currency) {
                    (Some(number), Some(currency)) => Some(Amount { number, currency }),
                    _ => None,
                },
                cost: w.cost,
                price: w.price,
                flag: w.flag,
                meta: w.meta,
            })
            .collect(),
        ..header
    };
    Outcome::Booked(txn, balance_error)
}

/// Gives a number without a currency the currency of the other postings, if
/// they agree on one.
fn infer_currencies(mut postings: Vec<RawPosting>) -> Vec<RawPosting> {
    let mut known: Vec<&str> = postings
        .iter()
        .filter_map(|p| p.currency.as_deref())
        .collect();
    known.sort_unstable();
    known.dedup();
    if let [only] = known.as_slice() {
        let only = (*only).to_owned();
        for posting in &mut postings {
            if posting.number.is_some() && posting.currency.is_none() {
                posting.currency = Some(only.clone());
            }
        }
    }
    postings
}

fn book_lots(
    postings: &[RawPosting],
    ctx: &Context<'_>,
    inventory: &Inventory,
) -> Result<Vec<Work>, BookingError> {
    let mut out = Vec::new();
    for p in postings {
        let price = price_per_unit(p)?;
        let base = Work {
            account: p.account.clone(),
            number: p.number,
            currency: p.currency.clone(),
            cost: None,
            price,
            flag: p.flag,
            meta: p.meta.clone(),
            line: p.line,
            reduces: None,
            auto: p.number.is_none()
                && p.currency.is_none()
                && p.cost.is_none()
                && p.price.is_none(),
        };
        let Some(spec) = &p.cost else {
            out.push(base);
            continue;
        };
        let (Some(units), Some(currency)) = (p.number, p.currency.as_deref()) else {
            return Err(error(
                ErrorKind::Unsupported,
                Some(p.line),
                "cost on a posting without units",
            ));
        };
        let lots = inventory
            .lots
            .get(&p.account)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let method = ctx
            .methods
            .get(&p.account)
            .copied()
            .unwrap_or(ctx.options.booking);
        let reducing = method != Booking::None
            && lots.iter().any(|l| {
                l.currency == currency && l.units.is_sign_negative() != units.is_sign_negative()
            });
        if reducing {
            reduce(&base, spec, lots, currency, units, method, &mut out)?;
        } else {
            let cost = new_lot(spec, p.line, units, ctx.date)?;
            out.push(Work {
                cost: Some(cost),
                ..base
            });
        }
    }
    Ok(out)
}

/// The cost of a new lot from its cost spec.
fn new_lot(spec: &CostSpec, line: usize, units: Decimal, date: Date) -> Result<Cost, BookingError> {
    let number = match (spec.per_unit, spec.total) {
        (Some(n), _) => n,
        (None, Some(total)) => number::div(total, units.abs()).ok_or_else(|| {
            error(
                ErrorKind::InterpolationError,
                Some(line),
                "zero units at total cost",
            )
        })?,
        (None, None) => {
            return Err(error(
                ErrorKind::Unsupported,
                Some(line),
                "augmentation without a cost",
            ));
        }
    };
    let Some(currency) = spec.currency.clone() else {
        return Err(error(
            ErrorKind::Unsupported,
            Some(line),
            "cost without a currency",
        ));
    };
    Ok(Cost {
        number,
        currency,
        date: spec.date.unwrap_or(date),
        label: spec.label.clone(),
    })
}

/// Splits a reducing posting across the lots it matches, by booking method.
fn reduce(
    base: &Work,
    spec: &CostSpec,
    lots: &[Lot],
    currency: &str,
    units: Decimal,
    method: Booking,
    out: &mut Vec<Work>,
) -> Result<(), BookingError> {
    let matches: Vec<(usize, &Lot)> = lots
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.currency == currency
                && l.units.is_sign_negative() != units.is_sign_negative()
                && spec.per_unit.is_none_or(|n| n == l.cost.number)
                && spec.currency.as_ref().is_none_or(|c| *c == l.cost.currency)
                && spec.date.is_none_or(|d| d == l.cost.date)
                && spec
                    .label
                    .as_ref()
                    .is_none_or(|s| Some(s) == l.cost.label.as_ref())
        })
        .collect();
    let wanted = units.abs();
    let available: Decimal = matches.iter().map(|(_, l)| l.units.abs()).sum();
    let ambiguous = || {
        error(
            ErrorKind::AmbiguousMatchError,
            None,
            format!("ambiguous or insufficient lots for {}", base.account),
        )
    };
    let chosen: Vec<(usize, &Lot)> = match method {
        Booking::Strict | Booking::None => {
            // One lot with enough units, or several that are used up exactly
            let single = matches.len() == 1 && wanted <= available;
            let exact = matches.len() > 1 && wanted == available;
            if !(single || exact) {
                return Err(ambiguous());
            }
            matches
        }
        Booking::Fifo | Booking::Lifo => {
            if wanted > available {
                return Err(ambiguous());
            }
            let mut sorted = matches;
            sorted.sort_by_key(|(i, l)| (l.cost.date, *i));
            if method == Booking::Lifo {
                sorted.reverse();
            }
            sorted
        }
    };
    let mut remaining = wanted;
    for (index, lot) in chosen {
        if remaining.is_zero() {
            break;
        }
        let take = remaining.min(lot.units.abs());
        remaining -= take;
        let signed = if units.is_sign_negative() {
            -take
        } else {
            take
        };
        out.push(Work {
            number: Some(signed),
            cost: Some(lot.cost.clone()),
            reduces: Some((index, signed)),
            ..base.clone()
        });
    }
    Ok(())
}

fn price_per_unit(p: &RawPosting) -> Result<Option<Amount>, BookingError> {
    let Some(spec) = &p.price else {
        return Ok(None);
    };
    let (Some(number), Some(currency)) = (spec.number, spec.currency.clone()) else {
        return Err(error(
            ErrorKind::Unsupported,
            Some(p.line),
            "price without a number or currency",
        ));
    };
    let number = if spec.total {
        let Some(units) = p.number else {
            return Err(error(
                ErrorKind::Unsupported,
                Some(p.line),
                "total price without units",
            ));
        };
        number::div(number, units.abs()).ok_or_else(|| {
            error(
                ErrorKind::InterpolationError,
                Some(p.line),
                "total price for zero units",
            )
        })?
    } else {
        number
    };
    Ok(Some(Amount { number, currency }))
}

/// The weight of a posting: what it contributes to the transaction's balance.
fn weight(w: &Work) -> Option<(Decimal, String)> {
    let number = w.number?;
    if let Some(cost) = &w.cost {
        return Some((number::mul(number, cost.number)?, cost.currency.clone()));
    }
    if let Some(price) = &w.price {
        return Some((number::mul(number, price.number)?, price.currency.clone()));
    }
    Some((number, w.currency.clone()?))
}

/// The currency a posting balances in.
fn group_currency(w: &Work) -> Option<String> {
    if let Some(cost) = &w.cost {
        return Some(cost.currency.clone());
    }
    if let Some(price) = &w.price {
        return Some(price.currency.clone());
    }
    w.currency.clone()
}

enum Interpolation {
    Drop(BookingError),
    Fail(BookingError),
}

fn interpolate(postings: Vec<Work>, options: &Options) -> Result<Vec<Work>, Interpolation> {
    let autos: Vec<&Work> = postings.iter().filter(|w| w.auto).collect();
    if autos.len() > 1 {
        return Err(Interpolation::Drop(error(
            ErrorKind::CategorizationError,
            Some(autos[1].line),
            "more than one posting without an amount",
        )));
    }
    let missing: Vec<&Work> = postings
        .iter()
        .filter(|w| !w.auto && w.number.is_none())
        .collect();
    if let Some(first) = missing.first()
        && (!autos.is_empty() || missing.len() > 1)
    {
        return Err(Interpolation::Fail(error(
            ErrorKind::InterpolationError,
            Some(first.line),
            "can't work out the amount",
        )));
    }

    // Group postings by the currency they balance in, in order of appearance
    let mut order: Vec<String> = Vec::new();
    for w in postings.iter().filter(|w| !w.auto) {
        if let Some(c) = group_currency(w)
            && !order.contains(&c)
        {
            order.push(c);
        }
    }
    let auto = postings
        .iter()
        .enumerate()
        .find(|(_, w)| w.auto)
        .map(|(i, w)| (i, w.clone()));
    let mut out = Vec::new();
    for currency in &order {
        let mut group: Vec<(usize, Work)> = postings
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.auto && group_currency(w).as_ref() == Some(currency))
            .map(|(i, w)| (i, w.clone()))
            .collect();
        let mut residual = Decimal::ZERO;
        for (_, w) in &group {
            if let Some((n, _)) = weight(w) {
                residual = number::add(residual, n).unwrap_or(residual);
            }
        }
        let works: Vec<Work> = group.iter().map(|(_, w)| w.clone()).collect();
        let default_places = options
            .tolerance_defaults
            .iter()
            .find(|(c, _)| c == currency)
            .map(|(_, t)| t.scale());
        let places = coarsest_places(&works, currency).or(default_places);
        if let Some((_, target)) = group.iter_mut().find(|(_, w)| w.number.is_none()) {
            target.number = Some(quantize(-residual, places));
            target.currency.get_or_insert_with(|| currency.clone());
        } else if let Some((index, auto)) = &auto
            && !residual.is_zero()
        {
            let mut filled = auto.clone();
            filled.auto = false;
            filled.number = Some(quantize(-residual, places));
            filled.currency = Some(currency.clone());
            group.push((*index, filled));
        }
        group.sort_by_key(|(i, _)| *i);
        out.extend(group.into_iter().map(|(_, w)| w));
    }
    // Postings that balance in no currency, such as a lone elided one
    out.extend(
        postings
            .into_iter()
            .filter(|w| !w.auto && group_currency(w).is_none()),
    );
    Ok(out)
}

/// Decimal places of the coarsest non-integer number in this currency.
fn coarsest_places(group: &[Work], currency: &str) -> Option<u32> {
    group
        .iter()
        .filter(|w| w.currency.as_deref() == Some(currency))
        .filter_map(|w| w.number)
        .map(number::decimal_places)
        .filter(|p| *p > 0)
        .min()
}

fn quantize(value: Decimal, places: Option<u32>) -> Decimal {
    let Some(places) = places else { return value };
    let mut rounded = value.round_dp_with_strategy(places, RoundingStrategy::MidpointNearestEven);
    rounded.rescale(places);
    // Python keeps the sign of a value that rounds to zero
    if rounded.is_zero() && value.is_sign_negative() {
        rounded.set_sign_negative(true);
    }
    rounded
}

/// The tolerance for each currency: the largest of half a unit in the last
/// place of each non-integer number in that currency, and the default.
pub(crate) fn tolerance(
    numbers: impl Iterator<Item = Decimal>,
    currency: &str,
    options: &Options,
) -> Decimal {
    let inferred = numbers
        .map(number::decimal_places)
        .filter(|p| *p > 0)
        .map(|p| number::unit(p) * options.tolerance_multiplier)
        .max()
        .unwrap_or(Decimal::ZERO);
    inferred.max(default_tolerance(currency, options))
}

pub(crate) fn default_tolerance(currency: &str, options: &Options) -> Decimal {
    options
        .tolerance_defaults
        .iter()
        .find(|(c, _)| c == currency)
        .or_else(|| options.tolerance_defaults.iter().find(|(c, _)| c == "*"))
        .map_or(Decimal::ZERO, |(_, t)| *t)
}

fn check_balanced(postings: &[Work], options: &Options) -> Option<BookingError> {
    let mut residuals: Vec<(String, Decimal)> = Vec::new();
    for w in postings {
        let Some((n, currency)) = weight(w) else {
            continue;
        };
        match residuals.iter_mut().find(|(c, _)| *c == currency) {
            Some((_, total)) => *total = number::add(*total, n).unwrap_or(*total),
            None => residuals.push((currency, n)),
        }
    }
    for (currency, residual) in residuals {
        let numbers = postings
            .iter()
            .filter(|w| w.currency.as_deref() == Some(currency.as_str()))
            .filter_map(|w| w.number);
        if residual.abs() > tolerance(numbers, &currency, options) {
            return Some(error(
                ErrorKind::ValidationError,
                None,
                format!("transaction doesn't balance: {residual} {currency}"),
            ));
        }
    }
    None
}

fn apply_lots(postings: &[Work], inventory: &mut Inventory) {
    let mut removals: HashMap<String, Vec<usize>> = HashMap::new();
    for w in postings {
        let (Some(cost), Some(units), Some(currency)) = (&w.cost, w.number, &w.currency) else {
            continue;
        };
        let lots = inventory.lots.entry(w.account.clone()).or_default();
        match w.reduces {
            Some((index, signed)) => {
                if let Some(lot) = lots.get_mut(index) {
                    lot.units += signed;
                    if lot.units.is_zero() {
                        removals.entry(w.account.clone()).or_default().push(index);
                    }
                }
            }
            None => {
                if let Some(lot) = lots
                    .iter_mut()
                    .find(|l| l.currency == *currency && l.cost == *cost)
                {
                    // Equal costs merge; the lot keeps the latest way the cost
                    // was written, such as 0.90 after 0.9
                    lot.units += units;
                    lot.cost = cost.clone();
                } else {
                    lots.push(Lot {
                        units,
                        currency: currency.clone(),
                        cost: cost.clone(),
                    });
                }
            }
        }
    }
    for (account, mut indices) in removals {
        indices.sort_unstable();
        indices.dedup();
        if let Some(lots) = inventory.lots.get_mut(&account) {
            for index in indices.into_iter().rev() {
                lots.remove(index);
            }
        }
    }
}
