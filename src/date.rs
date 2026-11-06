//! Calendar dates.

use std::fmt;

/// A calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// The year.
    pub year: u16,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, from 1.
    pub day: u8,
}

impl Date {
    /// A date, if it exists in the calendar.
    #[must_use]
    pub fn new(year: u16, month: u8, day: u8) -> Option<Date> {
        if year == 0 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    /// Parses `YYYY-MM-DD` or `YYYY/MM/DD`.
    pub(crate) fn parse(text: &str) -> Option<Date> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != bytes[7] || !matches!(bytes[4], b'-' | b'/') {
            return None;
        }
        Date::new(
            text[0..4].parse().ok()?,
            text[5..7].parse().ok()?,
            text[8..10].parse().ok()?,
        )
    }
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[cfg(test)]
mod tests {
    use super::Date;

    #[test]
    fn parses_valid_dates() {
        assert_eq!(
            Date::parse("2024-02-29").map(|d| d.to_string()),
            Some("2024-02-29".into())
        );
        assert_eq!(
            Date::parse("2024/01/05").map(|d| d.to_string()),
            Some("2024-01-05".into())
        );
    }

    #[test]
    fn rejects_impossible_dates() {
        assert_eq!(Date::parse("2023-02-29"), None);
        assert_eq!(Date::parse("2024-13-01"), None);
        assert_eq!(Date::parse("2024-01/01"), None);
    }
}
