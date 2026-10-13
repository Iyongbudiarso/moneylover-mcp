//! The JSON shapes tools accept and return. Doc comments on fields become the
//! JSON schema descriptions an LLM reads, so they are written for that reader.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Kind, Transaction, TxType, Wallet, fmt_money};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWalletsInput {
    /// Bypass the cache and fetch live balances (slow on large accounts).
    pub refresh: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCategoriesInput {
    /// Wallet name. Omit for the account-wide category list.
    pub wallet: Option<String>,
    /// Only return categories of this type.
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTransactionsInput {
    /// First day, inclusive, as YYYY-MM-DD.
    pub start_date: String,
    /// Last day, inclusive, as YYYY-MM-DD.
    pub end_date: String,
    /// Wallet name. Omit for all wallets.
    pub wallet: Option<String>,
    /// Only transactions in the category with this exact name (case-insensitive).
    pub category: Option<String>,
    /// Maximum rows to return, newest first. Default 50, maximum 200.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummarizeInput {
    /// First day, inclusive, as YYYY-MM-DD.
    pub start_date: String,
    /// Last day, inclusive, as YYYY-MM-DD.
    pub end_date: String,
    /// Wallet name. Omit for all wallets.
    pub wallet: Option<String>,
    /// Which side to total. Defaults to expense.
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
    /// Only needed when the category name exists as both income and expense.
    #[serde(rename = "type")]
    pub kind: Option<TxType>,
    /// If true, validate and resolve everything but do not create the transaction.
    pub dry_run: Option<bool>,
    /// Set to true only when the user explicitly wants a second transaction identical
    /// to one created in the last few minutes.
    pub allow_duplicate: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WalletOut {
    pub name: String,
    pub currency: Option<String>,
    /// Decimal string, to avoid floating point rounding.
    pub balance: Option<String>,
    pub archived: bool,
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
    pub name: String,
    /// "Parent > Child" for sub-categories, otherwise the same as name.
    pub full_name: String,
    #[serde(rename = "type")]
    pub kind: Kind,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CategoriesOut {
    pub categories: Vec<CategoryOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransactionOut {
    pub id: String,
    pub date: String,
    pub wallet: String,
    pub category: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Decimal string, always positive.
    pub amount: String,
    pub currency: Option<String>,
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
    pub transactions: Vec<TransactionOut>,
    /// Number of transactions that matched, before `limit` was applied.
    pub total_count: usize,
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
    pub category: String,
    pub total: String,
    pub count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CurrencySummary {
    pub currency: String,
    pub total: String,
    pub count: usize,
    /// Largest first.
    pub categories: Vec<CategoryTotal>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SummaryOut {
    pub start_date: String,
    pub end_date: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// One entry per currency; amounts in different currencies are never added together.
    pub currencies: Vec<CurrencySummary>,
    pub note: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddTransactionOut {
    /// True only if the transaction was really created.
    pub created: bool,
    pub dry_run: bool,
    /// Present when Money Lover returned an id for the new transaction.
    pub id: Option<String>,
    pub wallet: String,
    pub category: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub amount: String,
    pub currency: Option<String>,
    pub date: String,
    pub note: String,
}
