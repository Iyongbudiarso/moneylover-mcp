//! Checks on tool input, run before anything is sent to Money Lover.

use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde_json::Value;

use super::schema::AmountInput;
use crate::{
    error::{Error, Result},
    model::{API_DATE_FORMAT, parse_decimal},
};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_RANGE_DAYS: i64 = 366;
const MAX_NOTE_CHARS: usize = 500;
const MAX_DECIMAL_PLACES: u32 = 8;
const MAX_AMOUNT: i64 = 1_000_000_000_000_000;

#[derive(Debug, Clone, Copy)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    pub fn parse(start: &str, end: &str) -> Result<Self> {
        let range = Self {
            start: parse_date("start_date", start)?,
            end: parse_date("end_date", end)?,
        };
        if range.start > range.end {
            return Err(invalid(format!(
                "start_date {} is after end_date {}",
                range.start, range.end
            )));
        }
        if (range.end - range.start).num_days() >= MAX_RANGE_DAYS {
            return Err(invalid(format!(
                "date range is too large; use at most {MAX_RANGE_DAYS} days per call"
            )));
        }
        Ok(range)
    }
}

pub fn parse_date(field: &str, value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), API_DATE_FORMAT).map_err(|_| {
        invalid(format!(
            "{field} must be a date formatted YYYY-MM-DD, got '{value}'"
        ))
    })
}

pub fn today_in(timezone: Tz) -> NaiveDate {
    Utc::now().with_timezone(&timezone).date_naive()
}

/// The given date, or today in the user's timezone.
pub fn date_or_today(value: Option<&str>, timezone: Tz) -> Result<NaiveDate> {
    match value {
        Some(date) => parse_date("date", date),
        None => Ok(today_in(timezone)),
    }
}

pub fn limit(requested: Option<u32>) -> usize {
    requested
        .map_or(DEFAULT_LIMIT, |n| n as usize)
        .clamp(1, MAX_LIMIT)
}

pub fn parse_amount(input: &AmountInput) -> Result<Decimal> {
    let raw = match input {
        AmountInput::Number(n) => n.to_string(),
        AmountInput::Text(s) => s.trim().to_string(),
    };
    let amount = parse_decimal(&Value::String(raw.clone()))
        .ok_or_else(|| invalid(format!("amount '{raw}' is not a valid number")))?;
    if amount <= Decimal::ZERO {
        return Err(invalid(
            "amount must be greater than zero; income vs expense is decided by the category",
        ));
    }
    if amount.scale() > MAX_DECIMAL_PLACES || amount > Decimal::from(MAX_AMOUNT) {
        return Err(invalid(format!("amount '{raw}' is out of range")));
    }
    Ok(amount)
}

pub fn parse_note(note: Option<String>) -> Result<String> {
    let note = note.unwrap_or_default().trim().to_string();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(invalid(format!(
            "note is too long (maximum {MAX_NOTE_CHARS} characters)"
        )));
    }
    Ok(note)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fmt_money;

    #[test]
    fn amounts_must_be_positive_and_sane() {
        let text = |s: &str| AmountInput::Text(s.into());
        assert_eq!(fmt_money(parse_amount(&text(" 12.50 ")).unwrap()), "12.5");
        assert_eq!(
            fmt_money(parse_amount(&AmountInput::Number(serde_json::Number::from(50000))).unwrap()),
            "50000"
        );
        assert!(parse_amount(&text("0")).is_err());
        assert!(parse_amount(&text("-5")).is_err());
        assert!(parse_amount(&text("abc")).is_err());
        assert!(parse_amount(&text("1.123456789")).is_err());
        assert!(parse_amount(&text("99999999999999999999")).is_err());
    }

    #[test]
    fn date_ranges_are_validated() {
        assert!(DateRange::parse("2026-04-01", "2026-04-30").is_ok());
        assert!(DateRange::parse("2026-04-30", "2026-04-01").is_err());
        assert!(DateRange::parse("April 1", "2026-04-30").is_err());
        assert!(DateRange::parse("2026-02-30", "2026-03-30").is_err());
        assert!(DateRange::parse("2024-01-01", "2026-01-01").is_err());
    }

    #[test]
    fn limit_defaults_and_is_clamped() {
        assert_eq!(limit(None), 50);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(10_000)), 200);
    }

    #[test]
    fn notes_are_trimmed_and_bounded() {
        assert_eq!(parse_note(Some("  hi ".into())).unwrap(), "hi");
        assert_eq!(parse_note(None).unwrap(), "");
        assert!(parse_note(Some("x".repeat(501))).is_err());
    }
}
