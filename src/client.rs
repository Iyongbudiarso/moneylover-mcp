use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::NaiveDate;
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
    model::{Category, Transaction, Wallet, fmt_money, items},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_READ_RETRIES: u32 = 2;
const FIRST_RETRY_DELAY_MS: u64 = 500;
const TOKEN_REJECTED: i64 = 706;
const API_DATE_FORMAT: &str = "%Y-%m-%d";

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

    /// `wallets` supplies the currency of each transaction's wallet.
    pub async fn transactions(
        &self,
        wallet_id: Option<&str>,
        start: NaiveDate,
        end: NaiveDate,
        wallets: &[Wallet],
    ) -> Result<Vec<Transaction>> {
        let payload = json!({
            "walletId": wallet_id.unwrap_or("all"),
            "startDate": start.format(API_DATE_FORMAT).to_string(),
            "endDate": end.format(API_DATE_FORMAT).to_string(),
        });
        let body = self
            .post("/transaction/list", payload, Operation::Read)
            .await?;
        let currencies = currency_by_wallet(wallets);
        let timezone = self.cfg.timezone;
        Ok(parse_list(&body, "transactions", |row| {
            Transaction::from_value(row, timezone, &currencies)
        }))
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

fn created_id(body: &Value) -> Option<String> {
    let data = body.get("data")?;
    data.get("_id")
        .or_else(|| data.get("transaction")?.get("_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn parse_list<T>(body: &Value, nested_key: &str, parse: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    let data = body.get("data").unwrap_or(&Value::Null);
    let rows = items(data, nested_key);
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
