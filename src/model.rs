//! Tolerant parsers for Money Lover's (unofficial, undocumented) JSON.
//!
//! The API shape is reverse-engineered, so everything is read from
//! `serde_json::Value`: a missing optional field degrades the output instead of
//! breaking the whole call. Only the fields we cannot work without (ids, names,
//! amounts) are required.

use std::{collections::HashMap, str::FromStr};

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How Money Lover writes a calendar date, in requests and in responses.
pub const API_DATE_FORMAT: &str = "%Y-%m-%d";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Income,
    Expense,
    Unknown,
}

// Input-side counterpart of `Kind`: only the two values a caller may choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TxType {
    Income,
    Expense,
}

impl From<TxType> for Kind {
    fn from(t: TxType) -> Self {
        match t {
            TxType::Income => Kind::Income,
            TxType::Expense => Kind::Expense,
        }
    }
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Income => "income",
            Kind::Expense => "expense",
            Kind::Unknown => "unknown",
        }
    }

    /// Money Lover encodes the category type as 1 = income, 2 = expense.
    fn from_value(v: Option<&Value>) -> Self {
        let n = match v {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        };
        match n {
            Some(1) => Kind::Income,
            Some(2) => Kind::Expense,
            _ => Kind::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Wallet {
    pub id: String,
    pub name: String,
    pub currency: Option<String>,
    pub balance: Option<String>,
    pub archived: bool,
    pub exclude_total: bool,
}

#[derive(Debug, Clone)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub parent_id: Option<String>,
    /// Wallet this category row belongs to (`account`).
    pub wallet_id: Option<String>,
    /// Wallets this category is hidden in (`exclude_accounts`).
    pub excluded_wallets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: String,
    pub date: NaiveDate,
    /// Sort key (unix seconds), newest first.
    pub sort_key: i64,
    pub wallet_id: Option<String>,
    pub wallet_name: String,
    pub category_name: String,
    pub kind: Kind,
    pub amount: Decimal,
    pub currency: Option<String>,
    pub note: String,
    pub exclude_report: bool,
}

/// A transaction together with the JSON it came from. Money Lover's edit replaces
/// the whole transaction, so the fields this server does not manage (photo,
/// location, event, "with", ...) must be sent back exactly as they were.
#[derive(Debug, Clone)]
pub struct StoredTransaction {
    pub transaction: Transaction,
    pub category_id: Option<String>,
    pub raw: Value,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn bool_field(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

/// A reference to another object is either its id string or the object itself.
fn id_of(reference: &Value) -> Option<String> {
    match reference {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(_) => str_field(reference, "_id"),
        _ => None,
    }
}

/// `data` is normally an array; tolerate it being nested under a key.
pub fn items<'a>(data: &'a Value, nested_key: &str) -> &'a [Value] {
    match data {
        Value::Array(a) => a,
        Value::Object(_) => data
            .get(nested_key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    }
}

pub fn parse_decimal(v: &Value) -> Option<Decimal> {
    match v {
        Value::Number(n) => {
            let s = n.to_string();
            Decimal::from_str(&s)
                .or_else(|_| Decimal::from_scientific(&s))
                .ok()
        }
        Value::String(s) => Decimal::from_str(s.trim()).ok(),
        _ => None,
    }
}

pub fn fmt_money(d: Decimal) -> String {
    d.normalize().to_string()
}

impl Wallet {
    pub fn from_value(v: &Value) -> Option<Self> {
        if bool_field(v, "isDelete") {
            return None;
        }
        let (currency, balance) = parse_balance(v.get("balance"));
        Some(Self {
            id: str_field(v, "_id")?,
            name: str_field(v, "name")?,
            currency,
            balance,
            archived: bool_field(v, "archived"),
            exclude_total: bool_field(v, "exclude_total"),
        })
    }
}

/// `balance` is `[{"IDR": "12345.00"}]` in the observed responses; accept a bare map too.
fn parse_balance(v: Option<&Value>) -> (Option<String>, Option<String>) {
    let map = match v {
        Some(Value::Array(a)) => a.first().and_then(Value::as_object),
        Some(Value::Object(o)) => Some(o),
        _ => None,
    };
    let Some((code, amount)) = map.and_then(|m| m.iter().next()) else {
        return (None, None);
    };
    let balance = parse_decimal(amount).map(fmt_money);
    (Some(code.clone()), balance)
}

impl Category {
    pub fn is_usable_in(&self, wallet_id: &str) -> bool {
        self.wallet_id.as_deref() == Some(wallet_id)
            && !self.excluded_wallets.iter().any(|w| w == wallet_id)
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        if bool_field(v, "isDelete") {
            return None;
        }

        let excluded_wallets = v
            .get("exclude_accounts")
            .and_then(Value::as_array)
            .map(|refs| refs.iter().filter_map(id_of).collect())
            .unwrap_or_default();
        Some(Self {
            id: str_field(v, "_id")?,
            name: str_field(v, "name")?,
            kind: Kind::from_value(v.get("type")),
            parent_id: v.get("parent").and_then(id_of),
            wallet_id: str_field(v, "account"),
            excluded_wallets,
        })
    }
}

impl Transaction {
    pub fn from_value(
        v: &Value,
        tz: Tz,
        wallet_currency: &HashMap<String, String>,
    ) -> Option<Self> {
        if bool_field(v, "isDelete") {
            return None;
        }
        let account = v.get("account");
        let wallet_id = account.and_then(id_of);
        let wallet_name = account
            .and_then(|a| str_field(a, "name"))
            .unwrap_or_default();
        let category = v.get("category");
        let (date, sort_key) = v
            .get("displayDate")
            .and_then(Value::as_str)
            .and_then(|s| parse_date(s, tz))?;
        Some(Self {
            id: str_field(v, "_id")?,
            date,
            sort_key,
            currency: wallet_id
                .as_ref()
                .and_then(|id| wallet_currency.get(id))
                .cloned(),
            wallet_id,
            wallet_name,
            category_name: category
                .and_then(|c| str_field(c, "name"))
                .unwrap_or_default(),
            kind: Kind::from_value(category.and_then(|c| c.get("type"))),
            amount: parse_decimal(v.get("amount")?)?,
            note: v
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            exclude_report: bool_field(v, "exclude_report"),
        })
    }
}

impl StoredTransaction {
    pub fn from_value(
        v: &Value,
        tz: Tz,
        wallet_currency: &HashMap<String, String>,
    ) -> Option<Self> {
        Some(Self {
            transaction: Transaction::from_value(v, tz, wallet_currency)?,
            category_id: v.get("category").and_then(id_of),
            raw: v.clone(),
        })
    }

    /// Transfers, debts, loans and their repayments are tied to other
    /// transactions; changing one on its own would leave its counterpart wrong.
    pub fn is_linked(&self) -> bool {
        let is_set = |key: &str| match self.raw.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(_)) => true,
            _ => false,
        };
        let metadata = self
            .raw
            .get("category")
            .and_then(|c| str_field(c, "metadata"));
        is_set("related")
            || is_set("parent")
            || matches!(
                metadata.as_deref(),
                Some("IS_DEBT" | "IS_LOAN" | "IS_DEBT_COLLECTION" | "IS_REPAYMENT")
            )
    }
}

/// Returns the calendar date (in `tz`) and a sort key. A timestamp is converted to
/// the configured timezone; a bare `YYYY-MM-DD` is already a calendar date.
pub fn parse_date(s: &str, tz: Tz) -> Option<(NaiveDate, i64)> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some((dt.with_timezone(&tz).date_naive(), dt.timestamp()));
    }
    let d = NaiveDate::parse_from_str(s, API_DATE_FORMAT).ok()?;
    Some((d, d.and_hms_opt(0, 0, 0)?.and_utc().timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wallet_with_currency_and_balance() {
        let w = Wallet::from_value(&json!({
            "_id": "w1", "name": "Cash", "archived": false,
            "balance": [{"IDR": "1250000.00"}]
        }))
        .unwrap();
        assert_eq!(w.currency.as_deref(), Some("IDR"));
        assert_eq!(w.balance.as_deref(), Some("1250000"));
        assert!(!w.archived);
    }

    #[test]
    fn wallet_without_balance_or_deleted() {
        let w = Wallet::from_value(&json!({"_id": "w1", "name": "New"})).unwrap();
        assert!(w.currency.is_none() && w.balance.is_none());
        assert!(
            Wallet::from_value(&json!({"_id": "w1", "name": "Old", "isDelete": true})).is_none()
        );
        assert!(Wallet::from_value(&json!({"name": "no id"})).is_none());
    }

    #[test]
    fn category_kind_and_parent() {
        let c =
            Category::from_value(&json!({"_id": "c1", "name": "Food", "type": 2, "parent": "c0"}))
                .unwrap();
        assert_eq!(c.kind, Kind::Expense);
        assert_eq!(c.parent_id.as_deref(), Some("c0"));
        let c = Category::from_value(&json!({"_id": "c2", "name": "Salary", "type": "1"})).unwrap();
        assert_eq!(c.kind, Kind::Income);
        let c = Category::from_value(&json!({"_id": "c3", "name": "X", "type": 9})).unwrap();
        assert_eq!(c.kind, Kind::Unknown);
        let c = Category::from_value(
            &json!({"_id": "c4", "name": "Y", "type": 2, "account": "w1", "exclude_accounts": ["w2"]}),
        )
        .unwrap();
        assert_eq!(c.wallet_id.as_deref(), Some("w1"));
        assert_eq!(c.excluded_wallets, ["w2"]);
    }

    #[test]
    fn transaction_converts_timestamp_to_configured_timezone() {
        let currencies = HashMap::from([("w1".to_string(), "IDR".to_string())]);
        // 18:30 UTC on the 17th is 01:30 on the 18th in Jakarta (UTC+7).
        let t = Transaction::from_value(
            &json!({
                "_id": "t1", "amount": 50000, "note": "lunch",
                "displayDate": "2026-04-17T18:30:00.000Z",
                "account": {"_id": "w1", "name": "Cash"},
                "category": {"_id": "c1", "name": "Food", "type": 2}
            }),
            chrono_tz::Asia::Jakarta,
            &currencies,
        )
        .unwrap();
        assert_eq!(t.date, NaiveDate::from_ymd_opt(2026, 4, 18).unwrap());
        assert_eq!(t.currency.as_deref(), Some("IDR"));
        assert_eq!(t.kind, Kind::Expense);
        assert_eq!(fmt_money(t.amount), "50000");
    }

    #[test]
    fn transaction_needs_amount_and_date() {
        let none = HashMap::new();
        let base = json!({"_id": "t", "displayDate": "2026-04-18", "amount": "12.50"});
        assert!(Transaction::from_value(&base, chrono_tz::UTC, &none).is_some());
        assert!(
            Transaction::from_value(&json!({"_id": "t", "amount": 1}), chrono_tz::UTC, &none)
                .is_none()
        );
        assert!(
            Transaction::from_value(
                &json!({"_id": "t", "displayDate": "2026-04-18"}),
                chrono_tz::UTC,
                &none
            )
            .is_none()
        );
    }

    #[test]
    fn linked_transactions_are_recognised() {
        let none = HashMap::new();
        let stored = |extra: Value| {
            let mut v = json!({
                "_id": "t", "amount": 1, "displayDate": "2026-04-18",
                "category": {"_id": "c1", "name": "Food", "type": 2}
            });
            for (key, value) in extra.as_object().unwrap() {
                v[key] = value.clone();
            }
            StoredTransaction::from_value(&v, chrono_tz::UTC, &none).unwrap()
        };
        let plain = stored(json!({"related": "", "parent": null}));
        assert!(!plain.is_linked());
        assert_eq!(plain.category_id.as_deref(), Some("c1"));
        assert!(stored(json!({"related": "t2"})).is_linked());
        assert!(stored(json!({"related": true})).is_linked());
        assert!(stored(json!({"parent": {"_id": "t0"}})).is_linked());
        assert!(
            stored(json!({"category": {"_id": "c9", "name": "Loan", "metadata": "IS_LOAN"}}))
                .is_linked()
        );
    }

    #[test]
    fn decimals_from_numbers_and_strings() {
        assert_eq!(parse_decimal(&json!("12.50")), Some(Decimal::new(1250, 2)));
        assert_eq!(parse_decimal(&json!(7)), Some(Decimal::from(7)));
        assert_eq!(parse_decimal(&json!("abc")), None);
        assert_eq!(fmt_money(Decimal::new(1250, 2)), "12.5");
    }

    #[test]
    fn items_accepts_array_or_nested_object() {
        let arr = json!([{"a": 1}]);
        assert_eq!(items(&arr, "transactions").len(), 1);
        let obj = json!({"transactions": [{"a": 1}, {"a": 2}]});
        assert_eq!(items(&obj, "transactions").len(), 2);
        assert_eq!(items(&Value::Null, "transactions").len(), 0);
    }
}
