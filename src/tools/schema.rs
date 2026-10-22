//! The JSON shapes tools accept and return. Doc comments on fields become the
//! JSON schema descriptions an LLM reads, so they are written for that reader.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Kind, Transaction, TxType, Wallet, fmt_money};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWalletsInput {
    /// Set to true to bypass the short-lived cache and fetch live balances. Slow on large accounts,
    /// so leave it unset (default false) unless the user explicitly needs a live figure.
    pub refresh: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCategoriesInput {
    /// Wallet name, exactly as returned by list_wallets (case-insensitive). Omit for the account-wide category list.
    pub wallet: Option<String>,
    /// Filter by category type: "income" or "expense". Omit to return both.
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
}

impl ListCategoriesInput {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            None => "both income and expense",
            Some(TxType::Income) => "income only",
            Some(TxType::Expense) => "expense only",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTransactionsInput {
    /// First day of the range, inclusive, as YYYY-MM-DD in the user's timezone, e.g. "2026-04-01".
    pub start_date: String,
    /// Last day of the range, inclusive, as YYYY-MM-DD. Must not be before start_date; the whole
    /// range may span at most 366 days.
    pub end_date: String,
    /// Wallet name, exactly as returned by list_wallets. Omit for all wallets, including archived
    /// ones and those excluded from Money Lover's total.
    pub wallet: Option<String>,
    /// Only transactions whose category has this exact name (case-insensitive). For a sub-category
    /// use the child's own name; a "Parent > Child" path does not match here.
    pub category: Option<String>,
    /// Maximum rows to return, newest first. Default 50, maximum 200.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummarizeInput {
    /// First day of the range, inclusive, as YYYY-MM-DD in the user's timezone, e.g. "2026-04-01".
    pub start_date: String,
    /// Last day of the range, inclusive, as YYYY-MM-DD. Must not be before start_date; the whole
    /// range may span at most 366 days.
    pub end_date: String,
    /// Wallet name, exactly as returned by list_wallets. Omit to total across all wallets, including
    /// archived ones and those excluded from Money Lover's total.
    pub wallet: Option<String>,
    /// Which side to total: "expense" (spending) or "income" (earnings). Defaults to "expense".
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
}

/// A positive amount, as a number or a decimal string such as "50000" or "12.50".
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AmountInput {
    Number(serde_json::Number),
    Text(String),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddTransactionInput {
    /// Wallet name, exactly as returned by list_wallets.
    pub wallet: String,
    /// Category name as returned by list_categories. Sub-categories may be written as "Parent > Child".
    pub category: String,
    /// Positive amount in the wallet's currency, e.g. 50000 or "12.50". Never negative.
    pub amount: AmountInput,
    /// Free-text note.
    pub note: Option<String>,
    /// Transaction date as YYYY-MM-DD. Defaults to today in the user's timezone.
    pub date: Option<String>,
    /// "income" or "expense". Only needed when the category name exists as both; otherwise it is
    /// taken from the category.
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
    /// If true, validate and resolve everything but do not create the transaction.
    pub dry_run: Option<bool>,
    /// Set to true only when the user explicitly wants a second transaction identical
    /// to one created in the last few minutes.
    pub allow_duplicate: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTransactionInput {
    /// Id of the transaction, exactly as returned by list_transactions.
    pub id: String,
    /// Name of the wallet the transaction is in, exactly as returned by list_transactions.
    pub wallet: String,
    /// The transaction's current date as YYYY-MM-DD, exactly as returned by list_transactions.
    /// Used to find it; to move it to another day use new_date.
    pub date: String,
    /// New category name as returned by list_categories for the transaction's wallet. Sub-categories
    /// may be written as "Parent > Child". Omit to keep the current category.
    pub category: Option<String>,
    /// "income" or "expense". Only used with category, when that name exists as both.
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
    /// New positive amount in the wallet's currency, e.g. 50000 or "12.50". Omit to keep the current amount.
    pub amount: Option<AmountInput>,
    /// New note, replacing the old one entirely. Pass "" to clear it; omit to keep it.
    pub note: Option<String>,
    /// New date as YYYY-MM-DD. Omit to keep the current date.
    pub new_date: Option<String>,
    /// If true, find the transaction and validate the change, but do not save it.
    pub dry_run: Option<bool>,
}

impl UpdateTransactionInput {
    pub fn changes_something(&self) -> bool {
        self.category.is_some()
            || self.amount.is_some()
            || self.note.is_some()
            || self.new_date.is_some()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteTransactionInput {
    /// Id of the transaction, exactly as returned by list_transactions.
    pub id: String,
    /// Name of the wallet the transaction is in, exactly as returned by list_transactions.
    pub wallet: String,
    /// The transaction's date as YYYY-MM-DD, exactly as returned by list_transactions. Used to find it.
    pub date: String,
    /// If true, find the transaction and run every check, but do not delete it.
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WalletOut {
    /// Name to pass to other tools.
    pub name: String,
    /// ISO currency code of the wallet, e.g. IDR.
    pub currency: Option<String>,
    /// Decimal string, to avoid floating point rounding.
    pub balance: Option<String>,
    /// Archived wallets still appear in listings but reject new transactions.
    pub archived: bool,
    /// Excluded by the user from Money Lover's own total balance.
    pub excluded_from_total: bool,
}

impl From<&Wallet> for WalletOut {
    fn from(w: &Wallet) -> Self {
        Self {
            name: w.name.clone(),
            currency: w.currency.clone(),
            balance: w.balance.clone(),
            archived: w.archived,
            excluded_from_total: w.exclude_total,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WalletsOut {
    pub wallets: Vec<WalletOut>,
    /// True when served from the short-lived cache; balances may then be a few minutes old.
    pub from_cache: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CategoryOut {
    /// Name of this one level; a sub-category's own name, not the parent's.
    pub name: String,
    /// "Parent > Child" for sub-categories, otherwise the same as name.
    pub full_name: String,
    /// Whether transactions in this category count as income or expense.
    #[serde(rename = "type")]
    pub kind: Kind,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CategoriesOut {
    /// Filtered categories, sorted by type and then name.
    pub categories: Vec<CategoryOut>,
    /// Which types are included, so an empty list can be read correctly.
    pub kind: &'static str,
    /// The wallet the list applies to, or null when it is account-wide.
    pub wallet: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransactionOut {
    /// Pass together with wallet and date to update_transaction or delete_transaction.
    pub id: String,
    /// YYYY-MM-DD in the user's timezone.
    pub date: String,
    /// Name of the wallet the transaction is in, as list_wallets returns it.
    pub wallet: String,
    /// The category's own name; a sub-category appears without its parent.
    pub category: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Decimal string, always positive.
    pub amount: String,
    /// ISO currency code, or null when it could not be resolved.
    pub currency: Option<String>,
    /// Empty when no note was entered.
    pub note: String,
}

impl From<Transaction> for TransactionOut {
    fn from(t: Transaction) -> Self {
        Self {
            id: t.id,
            date: t.date.to_string(),
            wallet: t.wallet_name,
            category: t.category_name,
            kind: t.kind,
            amount: fmt_money(t.amount),
            currency: t.currency,
            note: t.note,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransactionsOut {
    /// Newest first, at most the requested limit.
    pub transactions: Vec<TransactionOut>,
    /// Number of transactions that matched, before `limit` was applied.
    pub total_count: usize,
    /// True when transactions were dropped to honour `limit`.
    pub truncated: bool,
}

impl TransactionsOut {
    pub fn first(transactions: Vec<Transaction>, limit: usize) -> Self {
        let total_count = transactions.len();
        Self {
            transactions: transactions
                .into_iter()
                .take(limit)
                .map(Into::into)
                .collect(),
            total_count,
            truncated: total_count > limit,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CategoryTotal {
    /// Category name; sub-categories are reported under their own name.
    pub category: String,
    /// Decimal string summed in this currency only.
    pub total: String,
    /// Transactions counted in this category.
    pub count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CurrencySummary {
    /// ISO currency code, or UNKNOWN when the wallet's currency could not be read.
    pub currency: String,
    /// Decimal string: the sum of every category below.
    pub total: String,
    /// Transactions counted in this currency.
    pub count: usize,
    /// Largest first.
    pub categories: Vec<CategoryTotal>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SummaryOut {
    /// First day of the range, inclusive.
    pub start_date: String,
    /// Last day of the range, inclusive.
    pub end_date: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// One entry per currency; amounts in different currencies are never added together.
    pub currencies: Vec<CurrencySummary>,
    /// Reminder that transactions flagged "exclude from report" are not counted.
    pub note: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddTransactionOut {
    /// True only if the transaction was really created.
    pub created: bool,
    /// True when nothing was sent to Money Lover.
    pub dry_run: bool,
    /// Present when Money Lover returned an id for the new transaction.
    pub id: Option<String>,
    /// Resolved wallet name, not necessarily in the case it was given.
    pub wallet: String,
    /// Resolved category with its type, e.g. "Food > Lunch (expense)".
    pub category: String,
    /// Income or expense, taken from the resolved category.
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Decimal string, always positive.
    pub amount: String,
    /// ISO currency code of the wallet.
    pub currency: Option<String>,
    /// The stored date, which is today when none was given.
    pub date: String,
    /// The stored note, trimmed; empty when none was given.
    pub note: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UpdateTransactionOut {
    /// True only if the change was really saved.
    pub updated: bool,
    /// True when nothing was sent to Money Lover.
    pub dry_run: bool,
    /// The transaction as it was before this call.
    pub before: TransactionOut,
    /// The transaction as it is (or, for a dry run, would be) after this call.
    pub after: TransactionOut,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeleteTransactionOut {
    /// True only if the transaction was really deleted.
    pub deleted: bool,
    /// True when nothing was sent to Money Lover.
    pub dry_run: bool,
    /// The transaction that was (or, for a dry run, would be) deleted.
    pub transaction: TransactionOut,
}
