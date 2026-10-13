//! Login and token refresh.
//!
//! Money Lover limits an account to roughly five devices and every fresh login
//! uses one slot, so tokens are persisted and refreshed instead of logging in
//! on every start. Refresh does not use a slot.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};

use crate::{
    config::Config,
    error::{Error, Result, api_error},
    http::send_json,
    token_store::{
        TokenLock, Tokens, clear_revoked, delete_tokens, is_revoked, load_tokens, lock_tokens,
        mark_revoked, save_tokens,
    },
};

/// Money Lover's error code for "this device was removed from the account".
pub const DEVICE_REMOVED: i64 = 717;
const LOCK_WAIT_SLACK: Duration = Duration::from_secs(10);

/// Refresh when the access token has less than a day left.
const REFRESH_MARGIN_SECS: i64 = 24 * 60 * 60;
/// Used when the server does not tell us (in a way we understand) when a token expires.
/// Longer than the refresh margin, so an unknown expiry does not mean a refresh on every
/// call; if the guess is too long, the server rejects the token and it is renewed then.
const FALLBACK_LIFETIME_SECS: i64 = 2 * REFRESH_MARGIN_SECS;
/// Anything above this is a timestamp in milliseconds rather than seconds.
const EPOCH_MILLIS_THRESHOLD: i64 = 100_000_000_000;
/// Anything above this (2001-09-09) is an absolute timestamp rather than a lifetime.
const EPOCH_SECONDS_THRESHOLD: i64 = 1_000_000_000;

/// What we know about the current access token when renewing it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CurrentToken {
    /// Close to expiry but still valid: keep using it if refreshing fails on the network.
    StillValid,
    /// The server already refused it.
    Rejected,
}

/// Step one of the login handshake.
struct LoginTicket {
    request_token: String,
    client_id: String,
}

pub struct Session {
    cfg: Arc<Config>,
    http: reqwest::Client,
    tokens: Option<Tokens>,
    /// Whether `tokens` came from, or were written to, the token file. If that file
    /// later disappears, someone logged out and the tokens must not be used again.
    tokens_on_disk: bool,
    /// Credentials only change on restart, so a rejected login is not retried: repeated
    /// attempts with a wrong password could get the account rate-limited or locked.
    login_rejected: Option<String>,
}

impl Session {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            tokens: None,
            tokens_on_disk: false,
            login_rejected: None,
        }
    }

    /// Returns a usable access token, refreshing or logging in only when needed.
    pub async fn access_token(&mut self) -> Result<String> {
        self.ensure_not_revoked()?;
        if let Some(t) = self.tokens.as_ref().filter(|t| is_fresh(t)) {
            return Ok(t.access_token.clone());
        }
        let _lock = self.lock().await?;
        // Another server process may have refreshed while we waited for the lock.
        self.reload_from_disk();
        self.ensure_not_revoked()?;
        match self.tokens.clone() {
            Some(t) if is_fresh(&t) => Ok(t.access_token),
            Some(t) => self.renew(t, CurrentToken::StillValid).await,
            None => self.login_and_store().await,
        }
    }

    /// Called after the server rejected `rejected_token`.
    pub async fn renew_rejected_token(&mut self, rejected_token: &str) -> Result<String> {
        self.ensure_not_revoked()?;
        let _lock = self.lock().await?;
        self.reload_from_disk();
        self.ensure_not_revoked()?;
        match self.tokens.clone() {
            Some(t) if t.access_token != rejected_token => Ok(t.access_token),
            Some(t) => self.renew(t, CurrentToken::Rejected).await,
            None => self.login_and_store().await,
        }
    }

    /// Explicit login (the `login` command): always asks Money Lover for new tokens,
    /// and lifts a previous logout or device removal.
    pub async fn log_in(&mut self) -> Result<()> {
        let _lock = self.lock().await?;
        let tokens = self.login().await?;
        save_tokens(&self.cfg.token_path, &tokens)?;
        clear_revoked(&self.cfg.token_path);
        self.tokens = Some(tokens);
        self.tokens_on_disk = true;
        Ok(())
    }

    /// The device was removed from the account. Every process sharing this token
    /// path stops until the user runs `login` again; nobody logs back in silently.
    pub fn revoke(&mut self) {
        self.tokens = None;
        mark_revoked(&self.cfg.token_path);
        delete_tokens(&self.cfg.token_path);
    }

    /// Checked on every call (a single `stat`), so `logout` or a removed device
    /// takes effect immediately, also in other server processes.
    fn ensure_not_revoked(&mut self) -> Result<()> {
        if !is_revoked(&self.cfg.token_path) {
            return Ok(());
        }
        self.tokens = None;
        Err(signed_out_error())
    }

    /// Waits for the token file at most as long as a peer process can need for one
    /// refresh-or-login round trip.
    async fn lock(&self) -> Result<TokenLock> {
        let max_wait = self.cfg.request_timeout * 2 + LOCK_WAIT_SLACK;
        lock_tokens(&self.cfg.token_path, max_wait).await
    }

    fn reload_from_disk(&mut self) {
        match load_tokens(&self.cfg.token_path) {
            Ok(Some(tokens)) => {
                self.tokens = Some(tokens);
                self.tokens_on_disk = true;
            }
            // The file we used is gone: someone logged out. Tokens that never made it
            // to disk (an unwritable token path) are kept.
            Ok(None) if self.tokens_on_disk => {
                self.tokens = None;
                self.tokens_on_disk = false;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("ignoring unreadable token file: {e}"),
        }
    }

    async fn renew(&mut self, current: Tokens, state: CurrentToken) -> Result<String> {
        let Some(refresh_token) = &current.refresh_token else {
            return self.login_and_store().await;
        };
        match self.refresh(refresh_token).await {
            Ok(fresh) => Ok(self.store(fresh)),
            Err(Error::Api {
                code: DEVICE_REMOVED,
                ..
            }) => {
                self.revoke();
                Err(signed_out_error())
            }
            // Only a definite "no" from the server justifies a login, which costs a device slot.
            Err(e @ Error::Auth(_)) => {
                tracing::warn!("token refresh failed ({e}); logging in again");
                self.login_and_store().await
            }
            Err(e) if state == CurrentToken::StillValid && seconds_left(&current) > 0 => {
                tracing::warn!("token refresh failed ({e}); using the current token");
                Ok(current.access_token)
            }
            Err(e) => Err(e),
        }
    }

    /// Memory first: a token file that cannot be written must not cost a login per call.
    fn store(&mut self, tokens: Tokens) -> String {
        let access_token = tokens.access_token.clone();
        self.tokens_on_disk = match save_tokens(&self.cfg.token_path, &tokens) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("could not save the token file, keeping tokens in memory only: {e}");
                false
            }
        };
        self.tokens = Some(tokens);
        access_token
    }

    async fn login_and_store(&mut self) -> Result<String> {
        if let Some(reason) = &self.login_rejected {
            return Err(Error::Auth(format!(
                "{reason} (not retried; fix the credentials and restart the MCP server)"
            )));
        }
        match self.login().await {
            Ok(tokens) => Ok(self.store(tokens)),
            Err(Error::Auth(reason)) => {
                self.login_rejected = Some(reason.clone());
                Err(Error::Auth(reason))
            }
            Err(e) => Err(e),
        }
    }

    async fn refresh(&self, refresh_token: &str) -> Result<Tokens> {
        let url = format!("{}/user/refresh-token", self.cfg.api_base);
        let request = self
            .http
            .post(url)
            .json(&json!({ "refreshToken": refresh_token }));
        let body = send_json(request).await?;
        match api_error(&body) {
            Some((DEVICE_REMOVED, message)) => {
                return Err(Error::Api {
                    code: DEVICE_REMOVED,
                    message,
                });
            }
            Some((code, message)) => {
                return Err(Error::Auth(format!("refresh rejected ({code}): {message}")));
            }
            None => {}
        }
        let data = body.get("data").unwrap_or(&body);
        tokens_from(data, Some(refresh_token))
            .ok_or_else(|| Error::Auth("refresh response contained no access token".into()))
    }

    async fn login(&self) -> Result<Tokens> {
        let (Some(email), Some(password)) = (&self.cfg.email, &self.cfg.password) else {
            return Err(Error::Auth(
                "no stored token and no credentials; set the email and password (MONEYLOVER_EMAIL, \
                 MONEYLOVER_PASSWORD) or run `moneylover-mcp login` once"
                    .into(),
            ));
        };
        tracing::info!("logging in to Money Lover (this uses one device slot)");
        let ticket = self.request_login_ticket().await?;
        self.exchange_credentials(&ticket, email, password).await
    }

    async fn request_login_ticket(&self) -> Result<LoginTicket> {
        let url = format!("{}/user/login-url", self.cfg.api_base);
        let body = send_json(self.http.post(url).json(&json!({}))).await?;
        let data = body.get("data").unwrap_or(&Value::Null);
        let request_token = data.get("request_token").and_then(Value::as_str);
        let client_id = data
            .get("login_url")
            .and_then(Value::as_str)
            .and_then(client_id_from_login_url);
        match (request_token, client_id) {
            (Some(request_token), Some(client_id)) => Ok(LoginTicket {
                request_token: request_token.to_string(),
                client_id,
            }),
            _ => Err(Error::Protocol(
                "login-url response had no request_token / client id".into(),
            )),
        }
    }

    async fn exchange_credentials(
        &self,
        ticket: &LoginTicket,
        email: &str,
        password: &str,
    ) -> Result<Tokens> {
        let url = format!("{}/token", self.cfg.oauth_base);
        let request = self
            .http
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", ticket.request_token))
            .header("client", &ticket.client_id)
            .form(&[("email", email), ("password", password)]);
        let body = send_json(request).await?;
        tokens_from(&body, None)
            .ok_or_else(|| Error::Auth(format!("login failed: {}", login_failure_reason(&body))))
    }
}

pub fn signed_out_error() -> Error {
    Error::Auth(
        "signed out (logout, or this device was removed from the Money Lover account); run `moneylover-mcp login` to sign in again".into(),
    )
}

fn seconds_left(tokens: &Tokens) -> i64 {
    tokens.expires_at.saturating_sub(Utc::now().timestamp())
}

fn is_fresh(tokens: &Tokens) -> bool {
    seconds_left(tokens) > REFRESH_MARGIN_SECS
}

fn client_id_from_login_url(login_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(login_url).ok()?;
    url.query_pairs()
        .find(|(key, _)| key == "client")
        .map(|(_, value)| value.into_owned())
}

fn login_failure_reason(body: &Value) -> String {
    if let Some((code, message)) = api_error(body) {
        return format!("({code}) {message}");
    }
    ["msg", "message", "error"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_str))
        .unwrap_or("no access token in response")
        .to_string()
}

fn tokens_from(data: &Value, fallback_refresh: Option<&str>) -> Option<Tokens> {
    let access_token = data.get("access_token")?.as_str()?.to_string();
    let refresh_token = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .or(fallback_refresh)
        .map(str::to_string);
    Some(Tokens {
        access_token,
        refresh_token,
        expires_at: parse_expiry(data.get("expire"), Utc::now().timestamp()),
    })
}

/// The `expire` field is not documented; accept epoch seconds, epoch millis,
/// seconds-from-now, or an RFC 3339 string, and fall back to a short lifetime.
pub fn parse_expiry(value: Option<&Value>, now: i64) -> i64 {
    match expiry_number(value) {
        Some(n) if n > EPOCH_MILLIS_THRESHOLD => n / 1000,
        Some(n) if n > EPOCH_SECONDS_THRESHOLD => n,
        Some(n) if n > 0 => now + n,
        _ => now + FALLBACK_LIFETIME_SECS,
    }
}

fn expiry_number(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>()
                .ok()
                .or_else(|| DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_formats() {
        let now = 1_800_000_000;
        assert_eq!(
            parse_expiry(Some(&json!(1_800_600_000)), now),
            1_800_600_000
        );
        assert_eq!(
            parse_expiry(Some(&json!(1_800_600_000_000_i64)), now),
            1_800_600_000
        );
        assert_eq!(parse_expiry(Some(&json!(604_800)), now), now + 604_800);
        assert_eq!(parse_expiry(Some(&json!("1800600000")), now), 1_800_600_000);
        assert_eq!(
            parse_expiry(Some(&json!("2027-01-15T08:00:00Z")), now),
            DateTime::parse_from_rfc3339("2027-01-15T08:00:00Z")
                .unwrap()
                .timestamp()
        );
        assert_eq!(parse_expiry(None, now), now + FALLBACK_LIFETIME_SECS);
        assert_eq!(
            parse_expiry(Some(&json!("soon")), now),
            now + FALLBACK_LIFETIME_SECS
        );
    }

    #[test]
    fn client_id_is_read_from_the_login_url() {
        assert_eq!(
            client_id_from_login_url("https://web.moneylover.me/login?client=abc&token=x")
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            client_id_from_login_url("https://web.moneylover.me/login"),
            None
        );
        assert_eq!(client_id_from_login_url("not a url"), None);
    }

    #[test]
    fn login_failure_reason_prefers_the_api_error() {
        assert_eq!(
            login_failure_reason(&json!({"error": 3, "msg": "wrong password"})),
            "(3) wrong password"
        );
        assert_eq!(login_failure_reason(&json!({"message": "nope"})), "nope");
        assert_eq!(
            login_failure_reason(&json!({})),
            "no access token in response"
        );
    }
}
