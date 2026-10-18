//! Tests against the real Money Lover API. Skipped unless explicitly requested:
//!
//!   MONEYLOVER_EMAIL=... MONEYLOVER_PASSWORD=... \
//!   cargo test --test live -- --ignored --nocapture --test-threads=1
//!
//! The read test never writes. The write test additionally needs
//! `MONEYLOVER_LIVE_WRITE=1` and only ever touches a wallet named "MCP Test":
//! it adds a transaction, edits it, then deletes it.
//! Output deliberately contains counts and field names only, never amounts or notes.

use chrono::{Duration, Utc};
use moneylover_mcp::{
    client::{Fetch, MoneyLover, NewTransaction},
    config::Config,
    resolve::{find_category, find_wallet},
};
use rust_decimal::Decimal;

fn client() -> MoneyLover {
    MoneyLover::new(Config::from_env().expect("config")).expect("client")
}

#[tokio::test]
#[ignore = "talks to the real Money Lover API"]
async fn live_read_only_smoke_test() {
    let ml = client();
    let tz = ml.config().timezone;

    let started = std::time::Instant::now();
    let wallets = match ml.wallets(Fetch::Fresh).await {
        Ok(w) => w.data,
        Err(e) => panic!(
            "wallets failed after {:.1}s: {e}",
            started.elapsed().as_secs_f32()
        ),
    };
    println!(
        "wallets: {} ({:.1}s)",
        wallets.len(),
        started.elapsed().as_secs_f32()
    );
    assert!(!wallets.is_empty(), "expected at least one wallet");
    for w in wallets.iter() {
        assert!(!w.name.is_empty() && !w.id.is_empty());
    }
    println!(
        "wallets with currency: {}",
        wallets.iter().filter(|w| w.currency.is_some()).count()
    );
    println!(
        "wallets with balance:  {}",
        wallets.iter().filter(|w| w.balance.is_some()).count()
    );

    let all = ml
        .categories_all(Fetch::Fresh)
        .await
        .expect("categories_all")
        .data;
    println!(
        "global categories: {} (unknown type: {})",
        all.len(),
        all.iter()
            .filter(|c| c.kind == moneylover_mcp::model::Kind::Unknown)
            .count()
    );
    assert!(!all.is_empty());

    let first = &wallets[0];
    let per_wallet = ml
        .categories_for_wallet(&first.id, Fetch::Cached)
        .await
        .expect("categories_for_wallet")
        .data;
    println!("categories in first wallet: {}", per_wallet.len());
    assert!(!per_wallet.is_empty());

    let today = Utc::now().with_timezone(&tz).date_naive();
    let txs = ml
        .transactions(None, today - Duration::days(30), today, &wallets)
        .await
        .expect("transactions");
    println!("transactions in the last 30 days: {}", txs.len());
    println!(
        "  with currency resolved: {}",
        txs.iter().filter(|t| t.currency.is_some()).count()
    );
    println!(
        "  with kind resolved:     {}",
        txs.iter()
            .filter(|t| t.kind != moneylover_mcp::model::Kind::Unknown)
            .count()
    );

    // Every wallet, archived and excluded-from-total ones too, must be in the combined list.
    let mut per_wallet = 0;
    for w in wallets.iter() {
        per_wallet += ml
            .transactions(Some(&w.id), today - Duration::days(30), today, &wallets)
            .await
            .expect("transactions per wallet")
            .len();
    }
    println!("  sum over wallets one by one: {per_wallet}");
    assert_eq!(
        txs.len(),
        per_wallet,
        "the all-wallets list misses some wallets"
    );
}

#[tokio::test]
#[ignore = "writes to the real Money Lover account (wallet 'MCP Test' only)"]
async fn live_write_to_mcp_test_wallet() {
    if std::env::var("MONEYLOVER_LIVE_WRITE").as_deref() != Ok("1") {
        println!("skipped: set MONEYLOVER_LIVE_WRITE=1 to run");
        return;
    }
    let ml = client();
    let wallets = ml.wallets(Fetch::Fresh).await.expect("wallets").data;
    let wallet = find_wallet(&wallets, "MCP Test").expect("create a wallet named 'MCP Test' first");

    let categories = ml
        .categories_for_wallet(&wallet.id, Fetch::Fresh)
        .await
        .expect("categories")
        .data;
    let category_name =
        std::env::var("MONEYLOVER_LIVE_CATEGORY").unwrap_or_else(|_| "Food & Beverage".into());
    let category = find_category(&categories, &category_name, None)
        .expect("set MONEYLOVER_LIVE_CATEGORY to an expense category that exists in the wallet");

    let category_id = category.id.clone();
    println!("categories in 'MCP Test': {}", categories.len());

    let today = Utc::now().with_timezone(&ml.config().timezone).date_naive();
    let marker = "moneylover-mcp live test";
    let count_marked = |txs: &[moneylover_mcp::model::Transaction]| {
        txs.iter().filter(|t| t.note.contains(marker)).count()
    };

    // An earlier run that failed or timed out may have left its transaction behind.
    // Remove it first (only marked transactions in 'MCP Test'), never add a duplicate.
    let leftovers = ml
        .transactions(Some(&wallet.id), today, today, &wallets)
        .await
        .expect("transactions");
    println!(
        "test transactions left over in 'MCP Test' today: {}",
        count_marked(&leftovers)
    );
    for t in leftovers.iter().filter(|t| t.note.contains(marker)) {
        ml.delete_transaction(&t.id)
            .await
            .expect("delete leftover test transaction");
    }
    let before = ml
        .transactions(Some(&wallet.id), today, today, &wallets)
        .await
        .expect("transactions");
    assert_eq!(
        count_marked(&before),
        0,
        "a leftover test transaction could not be deleted; delete it in the app and rerun"
    );

    let started = std::time::Instant::now();
    let result = ml
        .add_transaction(&NewTransaction {
            wallet_id: wallet.id.clone(),
            category_id,
            amount: Decimal::from(1234),
            note: format!("{marker} - safe to delete"),
            date: today,
        })
        .await;
    let took = started.elapsed().as_secs_f32();

    match &result {
        Ok(id) => println!(
            "add_transaction ok in {took:.1}s, server returned an id: {}",
            id.is_some()
        ),
        Err(e) => println!("add_transaction FAILED after {took:.1}s: {e}"),
    }

    // Whatever happened, look at what the server actually has.
    let after = ml
        .transactions(Some(&wallet.id), today, today, &wallets)
        .await
        .expect("transactions");
    let saved = count_marked(&after);
    println!("test transactions in 'MCP Test' now: {saved}");
    if result.is_err() && saved > 0 {
        println!("NOTE: the write reported an error but the transaction WAS saved");
    }
    result.expect("add_transaction");
    assert_eq!(
        saved, 1,
        "the new transaction was not found when listing the wallet"
    );

    // Edit the note, then delete the transaction, so the test cleans up after itself.
    let id = after
        .iter()
        .find(|t| t.note.contains(marker))
        .map(|t| t.id.clone())
        .unwrap();
    let stored = ml
        .transaction_by_id(&id, &wallet.id, today, &wallets)
        .await
        .expect("transaction_by_id")
        .expect("the new transaction is found by id");
    ml.edit_transaction(
        &stored,
        &NewTransaction {
            wallet_id: wallet.id.clone(),
            category_id: category.id.clone(),
            amount: Decimal::from(4321),
            note: format!("{marker} - edited, safe to delete"),
            date: today,
        },
    )
    .await
    .expect("edit_transaction");
    let edited = ml
        .transaction_by_id(&id, &wallet.id, today, &wallets)
        .await
        .expect("transaction_by_id")
        .expect("the edited transaction still exists");
    assert!(edited.transaction.note.contains("edited"));
    assert_eq!(edited.transaction.amount, Decimal::from(4321));
    println!("edit_transaction ok");

    ml.delete_transaction(&id)
        .await
        .expect("delete_transaction");
    let gone = ml
        .transaction_by_id(&id, &wallet.id, today, &wallets)
        .await
        .expect("transaction_by_id");
    assert!(
        gone.is_none(),
        "the transaction is still listed after delete"
    );
    println!("delete_transaction ok");
}
