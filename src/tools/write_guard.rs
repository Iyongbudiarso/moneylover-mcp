//! Limits on what an agent may write. The LLM decides which tools to call and
//! can be steered by text it reads (for example a note written by another member
//! of a shared wallet), so these checks are enforced here and not left to it.

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;

use crate::{
    config::Config,
    error::{Error, Result},
    model::{Wallet, fmt_money},
    resolve::normalized,
};

const RATE_WINDOW: Duration = Duration::from_secs(60 * 60);
/// An identical transaction within this window is treated as an accidental retry.
pub const DUPLICATE_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_DAYS_IN_PAST: u64 = 366;
const MAX_DAYS_IN_FUTURE: u64 = 31;

/// What makes two writes "the same transaction".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub wallet_id: String,
    pub category_id: String,
    pub amount: Decimal,
    pub date: NaiveDate,
    pub note: String,
}

struct Recorded {
    at: Instant,
    /// `None` for an update or delete, which only counts towards the hourly limit.
    fingerprint: Option<Fingerprint>,
}

pub struct WriteGuard {
    max_per_hour: usize,
    allowed_wallets: Option<Vec<String>>,
    max_amount: Option<Decimal>,
    recent: Mutex<VecDeque<Recorded>>,
}

impl WriteGuard {
    pub fn new(cfg: &Config) -> Self {
        Self {
            max_per_hour: cfg.max_writes_per_hour,
            allowed_wallets: cfg.write_wallets.clone(),
            max_amount: cfg.max_amount,
            recent: Mutex::new(VecDeque::new()),
        }
    }

    pub fn check_wallet(&self, wallet: &Wallet) -> Result<()> {
        let Some(allowed) = &self.allowed_wallets else {
            return Ok(());
        };
        if allowed.contains(&normalized(&wallet.name)) {
            return Ok(());
        }
        Err(refused(format!(
            "writing to wallet '{}' is not allowed by this server's configuration (MONEYLOVER_WRITE_WALLETS)",
            wallet.name
        )))
    }

    pub fn check_amount(&self, amount: Decimal) -> Result<()> {
        match self.max_amount {
            Some(max) if amount > max => Err(refused(format!(
                "amount {} is above the configured maximum of {} (MONEYLOVER_MAX_AMOUNT)",
                fmt_money(amount),
                fmt_money(max)
            ))),
            _ => Ok(()),
        }
    }

    /// Records the write before it is sent, so that even a write whose outcome is
    /// unknown (timeout) counts towards the limit and blocks an identical retry.
    pub fn admit(&self, fingerprint: Fingerprint, allow_duplicate: bool) -> Result<()> {
        let mut recent = self.recent();
        let is_duplicate = recent.iter().any(|r| {
            r.fingerprint.as_ref() == Some(&fingerprint) && r.at.elapsed() < DUPLICATE_WINDOW
        });
        if is_duplicate && !allow_duplicate {
            return Err(refused(format!(
                "an identical transaction was sent less than {} minutes ago, so this looks like an accidental retry. \
                 Check list_transactions first; if the user really wants a second identical transaction, call again with allow_duplicate=true",
                DUPLICATE_WINDOW.as_secs() / 60
            )));
        }
        self.record(&mut recent, Some(fingerprint))
    }

    /// Admits an update or delete. Repeating one changes nothing further, so only
    /// the hourly limit applies.
    pub fn admit_change(&self) -> Result<()> {
        let mut recent = self.recent();
        self.record(&mut recent, None)
    }

    /// Undoes `admit` for a write that certainly did not happen, so a corrected
    /// retry is neither refused as a duplicate nor counted against the limit.
    pub fn forget(&self, fingerprint: &Fingerprint) {
        self.forget_where(|r| r.fingerprint.as_ref() == Some(fingerprint));
    }

    /// Undoes `admit_change` for an update or delete that certainly did not happen.
    pub fn forget_change(&self) {
        self.forget_where(|r| r.fingerprint.is_none());
    }

    fn forget_where(&self, matches: impl Fn(&Recorded) -> bool) {
        let mut recent = self.recent();
        if let Some(index) = recent.iter().rposition(matches) {
            recent.remove(index);
        }
    }

    /// The writes of the last hour, oldest first.
    fn recent(&self) -> MutexGuard<'_, VecDeque<Recorded>> {
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while recent.front().is_some_and(|r| r.at.elapsed() > RATE_WINDOW) {
            recent.pop_front();
        }
        recent
    }

    fn record(
        &self,
        recent: &mut VecDeque<Recorded>,
        fingerprint: Option<Fingerprint>,
    ) -> Result<()> {
        if recent.len() >= self.max_per_hour {
            return Err(refused(format!(
                "write limit reached: at most {} writes (adds, updates and deletes) per hour (MONEYLOVER_MAX_WRITES_PER_HOUR)",
                self.max_per_hour
            )));
        }
        recent.push_back(Recorded {
            at: Instant::now(),
            fingerprint,
        });
        Ok(())
    }
}

/// Dates far from today are almost always a mistake (or an injected instruction).
pub fn check_date(date: NaiveDate, today: NaiveDate) -> Result<()> {
    let earliest = today - Days::new(MAX_DAYS_IN_PAST);
    let latest = today + Days::new(MAX_DAYS_IN_FUTURE);
    if date < earliest || date > latest {
        return Err(refused(format!(
            "date {date} is outside the allowed window ({earliest} to {latest})"
        )));
    }
    Ok(())
}

fn refused(message: String) -> Error {
    Error::Invalid(format!("refused: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(max_per_hour: usize) -> WriteGuard {
        let mut cfg = Config::from_lookup(|k| (k == "HOME").then(|| "/h".to_string())).unwrap();
        cfg.max_writes_per_hour = max_per_hour;
        cfg.write_wallets = Some(vec!["mcp test".into()]);
        cfg.max_amount = Some(Decimal::from(100));
        WriteGuard::new(&cfg)
    }

    fn fingerprint(note: &str) -> Fingerprint {
        Fingerprint {
            wallet_id: "w".into(),
            category_id: "c".into(),
            amount: Decimal::from(5),
            date: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            note: note.into(),
        }
    }

    fn wallet(name: &str) -> Wallet {
        Wallet {
            id: "w".into(),
            name: name.into(),
            currency: None,
            balance: None,
            archived: false,
            exclude_total: false,
        }
    }

    #[test]
    fn identical_writes_need_explicit_permission() {
        let g = guard(10);
        g.admit(fingerprint("a"), false).unwrap();
        let err = g.admit(fingerprint("a"), false).unwrap_err().to_string();
        assert!(err.contains("allow_duplicate"), "{err}");
        g.admit(fingerprint("a"), true).unwrap();
        g.admit(fingerprint("b"), false).unwrap();
    }

    #[test]
    fn forgetting_a_failed_write_frees_its_slot() {
        let g = guard(1);
        g.admit(fingerprint("a"), false).unwrap();
        g.forget(&fingerprint("a"));
        g.admit(fingerprint("a"), false).unwrap();
    }

    #[test]
    fn hourly_limit() {
        let g = guard(2);
        g.admit(fingerprint("1"), false).unwrap();
        g.admit(fingerprint("2"), false).unwrap();
        let err = g.admit(fingerprint("3"), false).unwrap_err().to_string();
        assert!(err.contains("write limit"), "{err}");
    }

    #[test]
    fn updates_and_deletes_share_the_hourly_limit() {
        let g = guard(2);
        g.admit(fingerprint("1"), false).unwrap();
        g.admit_change().unwrap();
        let err = g.admit_change().unwrap_err().to_string();
        assert!(err.contains("write limit"), "{err}");
        g.forget_change();
        g.admit_change().unwrap();
        // Forgetting a change never frees the slot of an add.
        g.forget_change();
        g.forget_change();
        g.admit(fingerprint("2"), false).unwrap();
        assert!(g.admit_change().is_err());
    }

    #[test]
    fn wallet_allowlist_and_max_amount() {
        let g = guard(10);
        assert!(g.check_wallet(&wallet("MCP  Test")).is_ok());
        assert!(g.check_wallet(&wallet("Savings")).is_err());
        assert!(g.check_amount(Decimal::from(100)).is_ok());
        assert!(g.check_amount(Decimal::from(101)).is_err());
    }

    #[test]
    fn date_window() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert!(check_date(today, today).is_ok());
        assert!(check_date(today - Days::new(366), today).is_ok());
        assert!(check_date(today - Days::new(367), today).is_err());
        assert!(check_date(today + Days::new(31), today).is_ok());
        assert!(check_date(today + Days::new(32), today).is_err());
        assert!(check_date(NaiveDate::from_ymd_opt(1, 1, 1).unwrap(), today).is_err());
    }
}
