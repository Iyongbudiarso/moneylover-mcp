# Changelog

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
