//! Totals per currency and category. Amounts in different currencies are never added together.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use super::schema::{CategoryTotal, CurrencySummary};
use crate::model::{Kind, Transaction, fmt_money};

const UNKNOWN_CURRENCY: &str = "UNKNOWN";

#[derive(Default, Clone, Copy)]
struct Tally {
    total: Decimal,
    count: usize,
}

impl Tally {
    // Saturating: amounts come from the server, and a corrupt value near
    // Decimal::MAX must not panic the whole tool call.
    fn add(&mut self, amount: Decimal) {
        self.total = self.total.saturating_add(amount);
        self.count += 1;
    }

    fn merge(self, other: Tally) -> Tally {
        Tally {
            total: self.total.saturating_add(other.total),
            count: self.count + other.count,
        }
    }
}

type TallyByCategory = BTreeMap<String, Tally>;

pub fn summarize(transactions: &[Transaction], kind: Kind) -> Vec<CurrencySummary> {
    group(transactions, kind)
        .into_iter()
        .map(|(currency, by_category)| currency_summary(currency, by_category))
        .collect()
}

/// Money Lover's "exclude from report" flag keeps a transaction out of totals.
fn counts_towards(transaction: &Transaction, kind: Kind) -> bool {
    transaction.kind == kind && !transaction.exclude_report
}

fn group(transactions: &[Transaction], kind: Kind) -> BTreeMap<String, TallyByCategory> {
    let mut by_currency: BTreeMap<String, TallyByCategory> = BTreeMap::new();
    for t in transactions.iter().filter(|t| counts_towards(t, kind)) {
        let currency = t
            .currency
            .clone()
            .unwrap_or_else(|| UNKNOWN_CURRENCY.into());
        by_currency
            .entry(currency)
            .or_default()
            .entry(t.category_name.clone())
            .or_default()
            .add(t.amount);
    }
    by_currency
}

fn currency_summary(currency: String, by_category: TallyByCategory) -> CurrencySummary {
    let overall = by_category
        .values()
        .fold(Tally::default(), |acc, t| acc.merge(*t));
    let mut categories: Vec<(String, Tally)> = by_category.into_iter().collect();
    categories
        .sort_by(|(name_a, a), (name_b, b)| b.total.cmp(&a.total).then_with(|| name_a.cmp(name_b)));
    CurrencySummary {
        currency,
        total: fmt_money(overall.total),
        count: overall.count,
        categories: categories
            .into_iter()
            .map(|(category, tally)| CategoryTotal {
                category,
                total: fmt_money(tally.total),
                count: tally.count,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn tx(
        category: &str,
        kind: Kind,
        amount: i64,
        currency: Option<&str>,
        excluded: bool,
    ) -> Transaction {
        Transaction {
            id: format!("{category}-{amount}"),
            date: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            sort_key: 0,
            wallet_id: None,
            wallet_name: String::new(),
            category_name: category.into(),
            kind,
            amount: Decimal::from(amount),
            currency: currency.map(str::to_string),
            note: String::new(),
            exclude_report: excluded,
        }
    }

    #[test]
    fn totals_per_currency_and_category_largest_first() {
        let txs = [
            tx("Food", Kind::Expense, 10, Some("IDR"), false),
            tx("Rent", Kind::Expense, 100, Some("IDR"), false),
            tx("Food", Kind::Expense, 5, Some("IDR"), false),
            tx("Food", Kind::Expense, 3, Some("USD"), false),
            tx("Salary", Kind::Income, 999, Some("IDR"), false),
            tx("Food", Kind::Expense, 50, Some("IDR"), true),
        ];
        let summary = summarize(&txs, Kind::Expense);
        assert_eq!(summary.len(), 2);
        let idr = &summary[0];
        assert_eq!(
            (idr.currency.as_str(), idr.total.as_str(), idr.count),
            ("IDR", "115", 3)
        );
        assert_eq!(idr.categories[0].category, "Rent");
        assert_eq!(
            (idr.categories[1].total.as_str(), idr.categories[1].count),
            ("15", 2)
        );
        assert_eq!(summary[1].currency, "USD");
    }

    #[test]
    fn missing_currency_is_grouped_as_unknown() {
        let summary = summarize(&[tx("Food", Kind::Expense, 1, None, false)], Kind::Expense);
        assert_eq!(summary[0].currency, UNKNOWN_CURRENCY);
    }
}
