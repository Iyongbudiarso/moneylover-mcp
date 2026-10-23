# Changelog

## 0.4.0

Transactions can now be corrected and deleted, not only added, so a mistake no longer has to be
fixed in the Money Lover app.

- `update_transaction` changes the category, amount, note or date of an existing transaction. Fields
  that are left out keep their value, and so do the photo, location, event, people and the
  "exclude from report" flag.
- `delete_transaction` deletes one transaction.
- Both take the `id`, `wallet` and `date` returned by `list_transactions`, support `dry_run`, and need
  `MONEYLOVER_ALLOW_WRITE=true`. The existing write limits apply to them: wallet allowlist, maximum
  amount, date window (for the transaction's current date as well as a new one) and the hourly
  limit. Transfers, debts, loans and repayments are refused, because changing one half would leave
  the linked transactions inconsistent.

Behaviour change: `MONEYLOVER_MAX_WRITES_PER_HOUR` now counts adds, updates and deletes together,
where it previously counted only added transactions.

### Fixed

- `list_transactions` and `summarize_spending` without a `wallet` silently left out every wallet
  excluded from Money Lover's total (and archived wallets), although they were documented as
  covering all wallets. On the account this was found on, that was 145 of 293 transactions in
  30 days. Those wallets are now fetched separately, so totals can go up after upgrading. This is
  slower on accounts with many such wallets; pass `wallet` to query only one.
- Money Lover returns at most 9,999 transactions per request and drops the rest without saying so.
  When the all-wallets request hits that cap the server now asks each wallet separately, and a
  single wallet at the cap is an error asking for a shorter range instead of an incomplete answer.

## 0.3.2

Tool descriptions rewritten against Glama's Tool Definition Quality Score (TDQS) rubric, which
scores what a description adds beyond the annotations and input schema. The descriptions now state
behaviour that was previously undocumented:

- Date ranges are capped at 366 days per call; `add_transaction` accepts dates from 366 days back
  to 31 days ahead.
- `summarize_spending` skips transactions flagged "exclude from report", while `list_transactions`
  still lists them.
- An account-wide `list_categories` collapses per-wallet duplicates, so a category can appear both
  as a parent and as a `"Parent > Child"` entry.
- `add_transaction` is append-only: no tool can edit or delete afterwards.
- `dry_run` runs name resolution and every write guard, and only skips the network call.

Each tool now names the tools it should not be confused with. Output schema fields have
descriptions, which clients show to the model alongside the input schema.

`list_categories` also gained two response fields, `kind` and `wallet`, so an empty list can be
told apart from a list filtered down to a type the wallet has none of. Both are additive.

## 0.3.1

Write mode can be switched on without the write tool disappearing from the tool list.

Also updates `rustls` to 0.23.46 for four RustSec advisories.

## 0.2.0

MCPB bundles and MCP Registry listing.

## 0.1.0

First release.
