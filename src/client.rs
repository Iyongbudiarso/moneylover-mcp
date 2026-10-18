use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use chrono::{Days, NaiveDate};
use reqwest::header::AUTHORIZATION;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::sync::Mutex;

pub use crate::cache::{Fetch, Loaded};
use crate::{
    auth::{DEVICE_REMOVED, Session, signed_out_error},
    cache::TtlCache,
    config::Config,
    error::{Error, Result, api_error},
    http::send_json,
    model::{Category, StoredTransaction, Transaction, Wallet, fmt_money, items},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_READ_RETRIES: u32 = 2;
const FIRST_RETRY_DELAY_MS: u64 = 500;
const TOKEN_REJECTED: i64 = 706;
const API_DATE_FORMAT: &str = "%Y-%m-%d";
/// `/transaction/list` returns at most this many rows and silently drops the rest
/// (observed on a live account). A full page is treated as possibly incomplete.
const MAX_TRANSACTION_ROWS: usize = 9999;

/// Decides whether a failed request may be sent again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    /// Safe to retry on network errors.
    Read,
    /// Never retried: the first attempt may have been saved, and a retry would duplicate it.
    Write,
}

pub struct NewTransaction {
    pub wallet_id: String,
    pub category_id: String,
    pub amount: Decimal,
    pub note: String,
    pub date: NaiveDate,
}

pub struct MoneyLover {
    cfg: Arc<Config>,
    http: reqwest::Client,
    /// Held for the whole duration of a request: calls are strictly sequential,
    /// because Money Lover resets connections under concurrency.
    session: Mutex<Session>,
    wallets: TtlCache<Arc<Vec<Wallet>>>,
    categories: TtlCache<Arc<Vec<Category>>>,
}

impl MoneyLover {
    pub fn new(cfg: Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("moneylover-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(cfg.request_timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            // The API never redirects. Following one could re-send the password or
            // refresh token (307/308 keep the body) to another host.
            .redirect(reqwest::redirect::Policy::none())
            .https_only(uses_https(&cfg))
            .build()
            .map_err(|e| Error::Config(format!("cannot build HTTP client: {e}")))?;
        let cfg = Arc::new(cfg);
        Ok(Self {
            session: Mutex::new(Session::new(cfg.clone(), http.clone())),
            wallets: TtlCache::new(cfg.cache_ttl),
            categories: TtlCache::new(cfg.cache_ttl),
            cfg,
            http,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Logs in with the configured credentials and stores the tokens.
    pub async fn log_in(&self) -> Result<()> {
        self.session.lock().await.log_in().await
    }

    /// `/wallet/list` is slow (tens of seconds on large accounts), hence the cache.
    pub async fn wallets(&self, mode: Fetch) -> Result<Loaded<Arc<Vec<Wallet>>>> {
        self.wallets
            .get(mode, || async {
                let body = self
                    .post("/wallet/list", json!({}), Operation::Read)
                    .await?;
                Ok(Arc::new(parse_list(&body, "wallets", Wallet::from_value)))
            })
            .await
    }

    /// Every category row of the account: one row per (category, wallet) pair.
    pub async fn categories_all(&self, mode: Fetch) -> Result<Loaded<Arc<Vec<Category>>>> {
        self.categories
            .get(mode, || async {
                let body = self
                    .post("/category/list-all", json!({}), Operation::Read)
                    .await?;
                Ok(Arc::new(parse_list(
                    &body,
                    "categories",
                    Category::from_value,
                )))
            })
            .await
    }

    /// Categories usable in one wallet.
    ///
    /// Verified against a live account: the ids from `/category/list` are never used
    /// by real transactions, and sending one to `/transaction/add` makes the request
    /// hang. Transactions use the `/category/list-all` rows whose `account` is the
    /// wallet, so that is the only source used here.
    pub async fn categories_for_wallet(
        &self,
        wallet_id: &str,
        mode: Fetch,
    ) -> Result<Loaded<Vec<Category>>> {
        let all = self.categories_all(mode).await?;
        Ok(all.map(|categories| {
            categories
                .iter()
                .filter(|c| c.is_usable_in(wallet_id))
                .cloned()
                .collect()
        }))
    }

    /// Transactions of one wallet, or of every wallet in `wallets` when `wallet_id`
    /// is `None`. `wallets` also supplies the currency of each transaction's wallet.
    pub async fn transactions(
        &self,
        wallet_id: Option<&str>,
        start: NaiveDate,
        end: NaiveDate,
        wallets: &[Wallet],
    ) -> Result<Vec<Transaction>> {
        let rows = match wallet_id {
            Some(id) => self.wallet_rows(id, start, end, wallets).await?,
            None => self.every_wallet_rows(start, end, wallets).await?,
        };
        let currencies = currency_by_wallet(wallets);
        let timezone = self.cfg.timezone;
        Ok(parse_rows(&rows, "transactions", |row| {
            Transaction::from_value(row, timezone, &currencies)
        }))
    }

    /// `walletId: "all"` answers in one request, but (verified against a live
    /// account) it leaves out wallets excluded from the total and stops at
    /// `MAX_TRANSACTION_ROWS`. Wallets it may leave out are fetched one by one; if
    /// it hit the limit, every wallet is. Rows seen twice are kept once.
    async fn every_wallet_rows(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        wallets: &[Wallet],
    ) -> Result<Vec<Value>> {
        let combined = self.transaction_rows(None, start, end).await?;
        let complete = combined.len() < MAX_TRANSACTION_ROWS;
        let mut rows = if complete { combined } else { Vec::new() };
        let one_by_one = wallets
            .iter()
            .filter(|w| !complete || w.exclude_total || w.archived);
        for wallet in one_by_one {
            rows.extend(self.wallet_rows(&wallet.id, start, end, wallets).await?);
        }
        let mut seen = HashSet::new();
        rows.retain(|row| match row.get("_id").and_then(Value::as_str) {
            Some(id) => seen.insert(id.to_string()),
            None => true,
        });
        Ok(rows)
    }

    /// A full page from a single wallet cannot be completed another way, so it is
    /// an error rather than a silently wrong list or total.
    async fn wallet_rows(
        &self,
        wallet_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        wallets: &[Wallet],
    ) -> Result<Vec<Value>> {
        let rows = self.transaction_rows(Some(wallet_id), start, end).await?;
        if rows.len() >= MAX_TRANSACTION_ROWS {
            let name = wallets
                .iter()
                .find(|w| w.id == wallet_id)
                .map_or("?", |w| w.name.as_str());
            return Err(Error::Invalid(format!(
                "wallet '{name}' has at least {MAX_TRANSACTION_ROWS} transactions between {start} and {end}, \
                 more than Money Lover returns at once; split the period into shorter date ranges"
            )));
        }
        Ok(rows)
    }

    /// Money Lover has no lookup by id, so the transaction is searched for in its
    /// wallet around the date it was listed with. One day either side absorbs
    /// timezone differences. The wallet is required: verified against a live
    /// account, `walletId: "all"` leaves out wallets excluded from the total.
    pub async fn transaction_by_id(
        &self,
        id: &str,
        wallet_id: &str,
        date: NaiveDate,
        wallets: &[Wallet],
    ) -> Result<Option<StoredTransaction>> {
        let start = date.checked_sub_days(Days::new(1)).unwrap_or(date);
        let end = date.checked_add_days(Days::new(1)).unwrap_or(date);
        let rows = self.transaction_rows(Some(wallet_id), start, end).await?;
        let currencies = currency_by_wallet(wallets);
        Ok(rows
            .iter()
            .filter(|row| row.get("_id").and_then(Value::as_str) == Some(id))
            .find_map(|row| StoredTransaction::from_value(row, self.cfg.timezone, &currencies)))
    }

    /// One `/transaction/list` request; `None` asks for `walletId: "all"`.
    async fn transaction_rows(
        &self,
        wallet_id: Option<&str>,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Value>> {
        let payload = json!({
            "walletId": wallet_id.unwrap_or("all"),
            "startDate": start.format(API_DATE_FORMAT).to_string(),
            "endDate": end.format(API_DATE_FORMAT).to_string(),
        });
        let body = self
            .post("/transaction/list", payload, Operation::Read)
            .await?;
        let data = body.get("data").unwrap_or(&Value::Null);
        Ok(items(data, "transactions").to_vec())
    }

    /// Creates a transaction and returns its id when the server reports one.
    pub async fn add_transaction(&self, new: &NewTransaction) -> Result<Option<String>> {
        let payload = json!({
            "with": [],
            "account": new.wallet_id,
            "category": new.category_id,
            "amount": fmt_money(new.amount),
            "note": new.note,
            "displayDate": new.date.format(API_DATE_FORMAT).to_string(),
        });
        let result = self
            .post("/transaction/add", payload, Operation::Write)
            .await;
        // Even a failed write (e.g. a timeout) may have been saved, so balances are suspect either way.
        self.wallets.clear().await;
        Ok(created_id(&result?))
    }

    /// Replaces `stored` with the values in `new`, keeping everything this server
    /// does not manage. `new.wallet_id` must be the transaction's own wallet.
    pub async fn edit_transaction(
        &self,
        stored: &StoredTransaction,
        new: &NewTransaction,
    ) -> Result<()> {
        let payload = edit_payload(stored, new);
        let result = self
            .post("/transaction/edit", payload, Operation::Write)
            .await;
        self.wallets.clear().await;
        result.map(drop)
    }

    /// Deletes one transaction. Linked transactions are never deleted along with it.
    pub async fn delete_transaction(&self, id: &str) -> Result<()> {
        let payload = json!({ "_id": id, "delRelated": false });
        let result = self
            .post("/transaction/delete", payload, Operation::Write)
            .await;
        self.wallets.clear().await;
        result.map(drop)
    }

    async fn post(&self, path: &str, body: Value, operation: Operation) -> Result<Value> {
        let mut session = self.session.lock().await;
        let mut token = session.access_token().await?;
        let mut retries = 0;
        let mut token_renewed = false;
        loop {
            let error = match self.post_once(path, &body, &token).await {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            if is_api_error(&error, DEVICE_REMOVED) {
                session.revoke();
                return Err(signed_out_error());
            }
            // A rejected token means nothing was done, so even a write can be resent.
            if is_api_error(&error, TOKEN_REJECTED) && !token_renewed {
                token_renewed = true;
                token = session.renew_rejected_token(&token).await?;
                continue;
            }
            // The request may have reached Money Lover before failing: say so instead of guessing.
            if operation == Operation::Write
                && matches!(error, Error::Transient(_) | Error::Protocol(_))
            {
                return Err(Error::OutcomeUnknown(error.to_string()));
            }
            if operation == Operation::Read && error.is_retryable() && retries < MAX_READ_RETRIES {
                retries += 1;
                let delay = retry_delay(retries);
                tracing::warn!(
                    "{path}: {error}; retrying in {delay:?} ({retries}/{MAX_READ_RETRIES})"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            return Err(error);
        }
    }

    async fn post_once(&self, path: &str, body: &Value, token: &str) -> Result<Value> {
        let request = self
            .http
            .post(format!("{}{}", self.cfg.api_base, path))
            .header(AUTHORIZATION, format!("AuthJWT {token}"))
            .json(body);
        let response = send_json(request).await?;
        match api_error(&response) {
            Some((code, message)) => Err(Error::Api { code, message }),
            None => Ok(response),
        }
    }
}

fn is_api_error(error: &Error, expected: i64) -> bool {
    matches!(error, Error::Api { code, .. } if *code == expected)
}

/// Production endpoints are https; only tests point at a local http mock.
fn uses_https(cfg: &Config) -> bool {
    cfg.api_base.starts_with("https://") && cfg.oauth_base.starts_with("https://")
}

/// 500 ms, 1 s, 2 s, ...
fn retry_delay(retry: u32) -> Duration {
    Duration::from_millis(FIRST_RETRY_DELAY_MS * 2u64.pow(retry - 1))
}

fn currency_by_wallet(wallets: &[Wallet]) -> HashMap<String, String> {
    wallets
        .iter()
        .filter_map(|w| Some((w.id.clone(), w.currency.clone()?)))
        .collect()
}

/// Mirrors what the Money Lover web app sends when a transaction is edited. The
/// edit overwrites every field, so the photo, location, event, people and
/// "exclude from report" flag are copied from the stored transaction unchanged.
fn edit_payload(stored: &StoredTransaction, new: &NewTransaction) -> Value {
    let raw = &stored.raw;
    let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    let number = |key: &str| {
        raw.get(key)
            .filter(|v| v.is_number())
            .cloned()
            .unwrap_or(json!(0))
    };
    // `address` is a JSON object serialized into a string.
    let address: Value = raw
        .get("address")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let with: Vec<&str> = raw
        .get("with")
        .and_then(Value::as_array)
        .map(|people| people.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let event = raw
        .get("campaign")
        .and_then(|c| c.get(0))
        .and_then(|c| match c {
            Value::String(id) => Some(id.clone()),
            _ => c.get("_id").and_then(Value::as_str).map(str::to_string),
        })
        .unwrap_or_default();

    let mut payload = json!({
        "_id": stored.transaction.id,
        "with": with,
        "account": new.wallet_id,
        "category": new.category_id,
        "amount": fmt_money(new.amount),
        "note": new.note,
        "displayDate": new.date.format(API_DATE_FORMAT).to_string(),
        "event": event,
        "exclude_report": stored.transaction.exclude_report,
        "longtitude": number("longtitude"),
        "latitude": number("latitude"),
        "addressName": text(address.get("name")),
        "addressDetails": text(address.get("details")),
        "addressIcon": text(address.get("icon")),
        "image": text(raw.get("images").and_then(|i| i.get(0))),
    });
    if let Some(remind) = raw.get("remind").filter(|r| !r.is_null()) {
        payload["remind"] = remind.clone();
    }
    payload
}

fn created_id(body: &Value) -> Option<String> {
    let data = body.get("data")?;
    data.get("_id")
        .or_else(|| data.get("transaction")?.get("_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn parse_list<T>(body: &Value, nested_key: &str, parse: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    let data = body.get("data").unwrap_or(&Value::Null);
    parse_rows(items(data, nested_key), nested_key, parse)
}

fn parse_rows<T>(rows: &[Value], nested_key: &str, parse: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    let parsed: Vec<T> = rows.iter().filter_map(parse).collect();
    let skipped = rows.len() - parsed.len();
    if skipped > 0 {
        tracing::debug!("skipped {skipped} unparsable or deleted item(s) in {nested_key}");
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles() {
        assert_eq!(retry_delay(1), Duration::from_millis(500));
        assert_eq!(retry_delay(2), Duration::from_millis(1000));
    }

    #[test]
    fn edit_payload_keeps_fields_it_does_not_change() {
        let raw = json!({
            "_id": "t1", "amount": 5, "note": "old", "displayDate": "2026-04-18T00:00:00.000Z",
            "account": {"_id": "w1", "name": "Cash"},
            "category": {"_id": "c1", "name": "Food", "type": 2},
            "with": ["Ann", "Bob"], "exclude_report": true,
            "longtitude": 106.8, "latitude": -6.2,
            "address": "{\"name\":\"Warung\",\"details\":\"Jl. X\",\"icon\":\"pin\"}",
            "images": ["photo.jpg"], "campaign": [{"_id": "e1", "type": 6}], "remind": 0
        });
        let stored = StoredTransaction::from_value(&raw, chrono_tz::UTC, &HashMap::new()).unwrap();
        let new = NewTransaction {
            wallet_id: "w1".into(),
            category_id: "c2".into(),
            amount: Decimal::new(1250, 2),
            note: "new".into(),
            date: NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
        };
        assert_eq!(
            edit_payload(&stored, &new),
            json!({
                "_id": "t1", "with": ["Ann", "Bob"], "account": "w1", "category": "c2",
                "amount": "12.5", "note": "new", "displayDate": "2026-04-19", "event": "e1",
                "exclude_report": true, "longtitude": 106.8, "latitude": -6.2,
                "addressName": "Warung", "addressDetails": "Jl. X", "addressIcon": "pin",
                "image": "photo.jpg", "remind": 0
            })
        );
    }

    #[test]
    fn edit_payload_defaults_missing_extras() {
        let raw = json!({"_id": "t1", "amount": 5, "displayDate": "2026-04-18"});
        let stored = StoredTransaction::from_value(&raw, chrono_tz::UTC, &HashMap::new()).unwrap();
        let new = NewTransaction {
            wallet_id: "w1".into(),
            category_id: "c1".into(),
            amount: Decimal::from(5),
            note: String::new(),
            date: NaiveDate::from_ymd_opt(2026, 4, 18).unwrap(),
        };
        let payload = edit_payload(&stored, &new);
        assert_eq!(payload["with"], json!([]));
        assert_eq!(payload["event"], "");
        assert_eq!(payload["image"], "");
        assert_eq!(payload["longtitude"], 0);
        assert_eq!(payload["exclude_report"], false);
        assert!(payload.get("remind").is_none());
    }

    #[test]
    fn created_id_accepts_both_response_shapes() {
        assert_eq!(
            created_id(&json!({"data": {"_id": "a"}})).as_deref(),
            Some("a")
        );
        assert_eq!(
            created_id(&json!({"data": {"transaction": {"_id": "b"}}})).as_deref(),
            Some("b")
        );
        assert_eq!(created_id(&json!({"data": null})), None);
    }
}
