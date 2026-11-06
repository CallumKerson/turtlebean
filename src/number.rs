//! Decimal arithmetic that matches beancount's: 28 significant digits,
//! rounding half to even, and exact results keeping their natural precision.

use rust_decimal::{Decimal, RoundingStrategy};

const PRECISION: u32 = 28;

/// Rounds to 28 significant digits, as Python's default decimal context does.
fn round(value: Decimal) -> Decimal {
    if digits(value) <= PRECISION {
        return value;
    }
    let excess = digits(value) - PRECISION;
    match value.scale().checked_sub(excess) {
        Some(places) => value.round_dp_with_strategy(places, RoundingStrategy::MidpointNearestEven),
        // Too large to round by dropping decimal places
        None => value,
    }
}

fn digits(value: Decimal) -> u32 {
    let mantissa = value.mantissa().unsigned_abs();
    if mantissa == 0 {
        1
    } else {
        mantissa.ilog10() + 1
    }
}

pub(crate) fn add(a: Decimal, b: Decimal) -> Option<Decimal> {
    a.checked_add(b).map(round)
}

pub(crate) fn sub(a: Decimal, b: Decimal) -> Option<Decimal> {
    a.checked_sub(b).map(round)
}

pub(crate) fn mul(a: Decimal, b: Decimal) -> Option<Decimal> {
    a.checked_mul(b).map(round)
}

/// Divides like Python's decimal module: an exact quotient keeps the scale
/// closest to `scale(a) - scale(b)`; an inexact one has 28 significant digits.
pub(crate) fn div(a: Decimal, b: Decimal) -> Option<Decimal> {
    if b.is_zero() {
        return None;
    }
    let quotient = a.checked_div(b)?;
    let ideal = i64::from(a.scale()) - i64::from(b.scale());
    // A quotient using every digit may have been rounded, even if multiplying
    // back appears to give `a`.
    let exact = digits(quotient.normalize()) < PRECISION
        && quotient.checked_mul(b).is_some_and(|product| product == a);
    if !exact {
        return Some(round(quotient));
    }
    let mut result = quotient.normalize();
    if i64::from(result.scale()) < ideal {
        result.rescale(u32::try_from(ideal).unwrap_or(0));
    }
    Some(result)
}

/// The number of digits after the decimal point, as written.
pub(crate) fn decimal_places(value: Decimal) -> u32 {
    value.scale()
}

/// One unit in the last place of a number with `places` decimal places.
pub(crate) fn unit(places: u32) -> Decimal {
    Decimal::new(1, places)
}

/// Parses a number literal, which may have thousands separators.
pub(crate) fn parse_literal(text: &str) -> Option<Decimal> {
    let cleaned: String = text.chars().filter(|c| *c != ',').collect();
    cleaned.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn inexact_division_has_28_significant_digits() {
        assert_eq!(
            div(d("10"), d("3")).unwrap().to_string(),
            "3.333333333333333333333333333"
        );
        assert_eq!(
            div(d("20"), d("3")).unwrap().to_string(),
            "6.666666666666666666666666667"
        );
    }

    #[test]
    fn exact_division_keeps_natural_scale() {
        assert_eq!(div(d("4"), d("2")).unwrap().to_string(), "2");
        assert_eq!(div(d("86.00"), d("100.00")).unwrap().to_string(), "0.86");
        assert_eq!(div(d("2000.00"), d("10")).unwrap().to_string(), "200.00");
        assert_eq!(
            div(d("85.129"), d("100.00")).unwrap().to_string(),
            "0.85129"
        );
    }

    #[test]
    fn division_by_zero_is_none() {
        assert_eq!(div(d("1"), d("0")), None);
    }

    #[test]
    fn multiplication_is_exact_when_it_fits() {
        assert_eq!(
            mul(d("29.23"), d("192.1114959716797")).unwrap().to_string(),
            "5615.419027252197631"
        );
        assert_eq!(
            mul(d("100.00"), d("0.8512")).unwrap().to_string(),
            "85.120000"
        );
    }

    #[test]
    fn literals_allow_thousands_separators() {
        assert_eq!(parse_literal("1,234.56"), Some(d("1234.56")));
    }
}
