use std::{fmt, ops::RangeInclusive, path::PathBuf, time::Duration};

use chrono_tz::Tz;
use rust_decimal::Decimal;

use crate::{
    error::{Error, Result},
    resolve::normalized,
};

pub const DEFAULT_API_BASE: &str = "https://web.moneylover.me/api";
pub const DEFAULT_OAUTH_BASE: &str = "https://oauth.moneylover.me";
const DEFAULT_TIMEZONE: &str = "Asia/Jakarta";
const DEFAULT_TIMEOUT_SECS: u64 = 45;
const TIMEOUT_SECS_RANGE: RangeInclusive<u64> = 1..=300;
const DEFAULT_CACHE_SECS: u64 = 300;
const CACHE_SECS_RANGE: RangeInclusive<u64> = 0..=24 * 60 * 60;
const DEFAULT_MAX_WRITES_PER_HOUR: usize = 20;
const MAX_WRITES_PER_HOUR_RANGE: RangeInclusive<usize> = 1..=1000;

#[derive(Clone)]
pub struct Config {
    pub email: Option<String>,
    pub password: Option<String>,
    /// Write tools are not registered unless this is true.
    pub allow_write: bool,
    pub timezone: Tz,
    pub token_path: PathBuf,
    pub request_timeout: Duration,
    /// How long wallets and categories are kept in memory. Zero disables the cache.
    pub cache_ttl: Duration,
    /// Upper bound on created transactions per rolling hour (runaway-agent guard).
    pub max_writes_per_hour: usize,
    /// When set, writes are only allowed into these wallets (normalized names).
    pub write_wallets: Option<Vec<String>>,
    /// When set, larger amounts are refused.
    pub max_amount: Option<Decimal>,
    pub api_base: String,
    pub oauth_base: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let env = |key: &str| get(key).filter(|v| !v.is_empty() && !is_unfilled_placeholder(v));
        Ok(Self {
            email: env("MONEYLOVER_EMAIL"),
            password: env("MONEYLOVER_PASSWORD"),
            allow_write: env("MONEYLOVER_ALLOW_WRITE").is_some_and(|v| is_truthy(&v)),
            timezone: parse_timezone(env("MONEYLOVER_TIMEZONE"))?,
            token_path: token_path(
                env("MONEYLOVER_TOKEN_PATH"),
                env("HOME").or_else(|| env("USERPROFILE")),
            )?,
            request_timeout: parse_seconds(
                "MONEYLOVER_TIMEOUT_SECS",
                env("MONEYLOVER_TIMEOUT_SECS"),
                DEFAULT_TIMEOUT_SECS,
                TIMEOUT_SECS_RANGE,
            )?,
            cache_ttl: parse_seconds(
                "MONEYLOVER_CACHE_SECS",
                env("MONEYLOVER_CACHE_SECS"),
                DEFAULT_CACHE_SECS,
                CACHE_SECS_RANGE,
            )?,
            max_writes_per_hour: parse_count(
                "MONEYLOVER_MAX_WRITES_PER_HOUR",
                env("MONEYLOVER_MAX_WRITES_PER_HOUR"),
                DEFAULT_MAX_WRITES_PER_HOUR,
                MAX_WRITES_PER_HOUR_RANGE,
            )?,
            write_wallets: env("MONEYLOVER_WRITE_WALLETS")
                .map(|v| parse_wallet_allowlist(&v))
                .transpose()?,
            max_amount: parse_max_amount(env("MONEYLOVER_MAX_AMOUNT"))?,
            api_base: DEFAULT_API_BASE.to_string(),
            oauth_base: DEFAULT_OAUTH_BASE.to_string(),
        })
    }
}

/// Some MCP clients (MCPB bundles) pass `${user_config.x}` through literally when the
/// user left an optional field empty; that means "not set", not a value.
fn is_unfilled_placeholder(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("${") && value.ends_with('}')
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_timezone(value: Option<String>) -> Result<Tz> {
    let name = value.unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
    name.trim().parse().map_err(|_| {
        Error::Config(format!(
            "MONEYLOVER_TIMEZONE '{name}' is not a valid IANA timezone (e.g. Asia/Jakarta)"
        ))
    })
}

fn token_path(explicit: Option<String>, home: Option<String>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        // A relative path would land in whatever directory the MCP client starts us in,
        // quite possibly a project repository.
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(Error::Config(
                "MONEYLOVER_TOKEN_PATH must be an absolute path".into(),
            ));
        }
        return Ok(path);
    }
    let home = home.ok_or_else(|| {
        Error::Config("cannot determine the home directory; set MONEYLOVER_TOKEN_PATH".into())
    })?;
    Ok(PathBuf::from(home).join(".config/moneylover-mcp/token.json"))
}

fn parse_seconds(
    name: &str,
    value: Option<String>,
    default: u64,
    allowed: RangeInclusive<u64>,
) -> Result<Duration> {
    parse_count(name, value, default, allowed).map(Duration::from_secs)
}

fn parse_count<T>(
    name: &str,
    value: Option<String>,
    default: T,
    allowed: RangeInclusive<T>,
) -> Result<T>
where
    T: std::str::FromStr + PartialOrd + fmt::Display,
{
    let Some(raw) = value else {
        return Ok(default);
    };
    match raw.trim().parse::<T>() {
        Ok(n) if allowed.contains(&n) => Ok(n),
        _ => Err(Error::Config(format!(
            "{name} '{raw}' must be a whole number between {} and {}",
            allowed.start(),
            allowed.end()
        ))),
    }
}

/// Comma-separated wallet names, compared case-insensitively.
fn parse_wallet_allowlist(value: &str) -> Result<Vec<String>> {
    let names: Vec<String> = value
        .split(',')
        .map(normalized)
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return Err(Error::Config(
            "MONEYLOVER_WRITE_WALLETS contains no wallet names; unset it to allow all wallets, \
             or unset MONEYLOVER_ALLOW_WRITE to allow none"
                .into(),
        ));
    }
    Ok(names)
}

fn parse_max_amount(value: Option<String>) -> Result<Option<Decimal>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    match raw.trim().parse::<Decimal>() {
        Ok(amount) if amount > Decimal::ZERO => Ok(Some(amount)),
        _ => Err(Error::Config(format!(
            "MONEYLOVER_MAX_AMOUNT '{raw}' must be a positive number"
        ))),
    }
}

// Manual Debug so the password can never end up in a log line.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("email", &self.email)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("allow_write", &self.allow_write)
            .field("timezone", &self.timezone)
            .field("token_path", &self.token_path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn defaults_are_safe() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/home/u")])).unwrap();
        assert!(!cfg.allow_write);
        assert_eq!(cfg.timezone, chrono_tz::Asia::Jakarta);
        assert_eq!(
            cfg.token_path,
            PathBuf::from("/home/u/.config/moneylover-mcp/token.json")
        );
        assert!(cfg.email.is_none() && cfg.password.is_none());
    }

    #[test]
    fn overrides_are_respected() {
        // Absolute on every OS ("/tmp/..." is not absolute on Windows).
        let token_path = std::env::temp_dir().join("t.json");
        let token_path_str = token_path.to_string_lossy().to_string();
        let cfg = Config::from_lookup(lookup(&[
            ("MONEYLOVER_ALLOW_WRITE", "TRUE"),
            ("MONEYLOVER_TIMEZONE", "Europe/Amsterdam"),
            ("MONEYLOVER_TOKEN_PATH", token_path_str.as_str()),
            ("MONEYLOVER_EMAIL", "a@b.c"),
        ]))
        .unwrap();
        assert!(cfg.allow_write);
        assert_eq!(cfg.timezone, chrono_tz::Europe::Amsterdam);
        assert_eq!(cfg.token_path, token_path);
        assert_eq!(cfg.email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn rejects_bad_timezone_and_missing_home() {
        assert!(
            Config::from_lookup(lookup(&[
                ("HOME", "/h"),
                ("MONEYLOVER_TIMEZONE", "Mars/Base")
            ]))
            .is_err()
        );
        assert!(Config::from_lookup(lookup(&[])).is_err());
    }

    #[test]
    fn timeout_default_and_validation() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/h")])).unwrap();
        assert_eq!(cfg.request_timeout, Duration::from_secs(45));
        let cfg = Config::from_lookup(lookup(&[("HOME", "/h"), ("MONEYLOVER_TIMEOUT_SECS", "90")]))
            .unwrap();
        assert_eq!(cfg.request_timeout, Duration::from_secs(90));
        for bad in ["0", "301", "abc", "-5"] {
            assert!(
                Config::from_lookup(lookup(&[("HOME", "/h"), ("MONEYLOVER_TIMEOUT_SECS", bad)]))
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn cache_default_and_validation() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/h")])).unwrap();
        assert_eq!(cfg.cache_ttl, Duration::from_secs(300));
        let cfg =
            Config::from_lookup(lookup(&[("HOME", "/h"), ("MONEYLOVER_CACHE_SECS", "0")])).unwrap();
        assert_eq!(cfg.cache_ttl, Duration::ZERO);
        for bad in ["-1", "abc", "86401"] {
            assert!(
                Config::from_lookup(lookup(&[("HOME", "/h"), ("MONEYLOVER_CACHE_SECS", bad)]))
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn write_limits() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/h")])).unwrap();
        assert_eq!(cfg.max_writes_per_hour, 20);
        assert!(cfg.write_wallets.is_none() && cfg.max_amount.is_none());

        let cfg = Config::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("MONEYLOVER_MAX_WRITES_PER_HOUR", "5"),
            ("MONEYLOVER_WRITE_WALLETS", " Cash ,  MCP  Test,,"),
            ("MONEYLOVER_MAX_AMOUNT", "1000000"),
        ]))
        .unwrap();
        assert_eq!(cfg.max_writes_per_hour, 5);
        assert_eq!(cfg.write_wallets.unwrap(), ["cash", "mcp test"]);
        assert_eq!(cfg.max_amount, Some(Decimal::from(1_000_000)));

        for (key, bad) in [
            ("MONEYLOVER_MAX_WRITES_PER_HOUR", "0"),
            ("MONEYLOVER_MAX_AMOUNT", "-1"),
            ("MONEYLOVER_MAX_AMOUNT", "lots"),
            ("MONEYLOVER_TOKEN_PATH", "relative/token.json"),
            ("MONEYLOVER_WRITE_WALLETS", " , ,"),
        ] {
            assert!(
                Config::from_lookup(lookup(&[("HOME", "/h"), (key, bad)])).is_err(),
                "{key}={bad}"
            );
        }
    }

    #[test]
    fn unfilled_client_placeholders_count_as_unset() {
        let cfg = Config::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("MONEYLOVER_EMAIL", "${user_config.email}"),
            ("MONEYLOVER_TIMEZONE", "${user_config.timezone}"),
        ]))
        .unwrap();
        assert!(cfg.email.is_none());
        assert_eq!(cfg.timezone, chrono_tz::Asia::Jakarta);
    }

    #[test]
    fn debug_hides_password() {
        let cfg = Config::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("MONEYLOVER_PASSWORD", "hunter2"),
        ]))
        .unwrap();
        assert!(!format!("{cfg:?}").contains("hunter2"));
    }
}
