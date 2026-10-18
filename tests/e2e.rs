//! End-to-end tests: a real MCP client talks to our server over an in-memory pipe,
//! and Money Lover is replaced by a wiremock server.

use std::sync::Arc;

use moneylover_mcp::{
    client::MoneyLover,
    config::Config,
    token_store::{Tokens, load_tokens, save_tokens},
    tools::MoneyLoverServer,
};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, body_string_contains, header, method, path},
};

const FAR_FUTURE: i64 = 4_000_000_000;

/// Writes are only accepted close to today, so write tests use yesterday.
fn recent_date() -> String {
    (chrono::Utc::now().date_naive() - chrono::Days::new(1)).to_string()
}

struct Harness {
    mock: MockServer,
    dir: TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self {
            mock: MockServer::start().await,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn token_path(&self) -> std::path::PathBuf {
        self.dir.path().join("token.json")
    }

    fn seed_token(&self, access: &str, refresh: &str, expires_at: i64) {
        save_tokens(
            &self.token_path(),
            &Tokens {
                access_token: access.into(),
                refresh_token: Some(refresh.into()),
                expires_at,
            },
        )
        .unwrap();
    }

    fn config(&self, allow_write: bool, credentials: bool) -> Config {
        let token_path = self.token_path();
        let home = self.dir.path().to_string_lossy().to_string();
        let mut cfg = Config::from_lookup(|key| match key {
            "HOME" => Some(home.clone()),
            "MONEYLOVER_TIMEZONE" => Some("Asia/Jakarta".into()),
            "MONEYLOVER_ALLOW_WRITE" => allow_write.then(|| "true".into()),
            "MONEYLOVER_EMAIL" => credentials.then(|| "me@example.com".into()),
            "MONEYLOVER_PASSWORD" => credentials.then(|| "hunter2".into()),
            _ => None,
        })
        .unwrap();
        cfg.token_path = token_path;
        cfg.api_base = format!("{}/api", self.mock.uri());
        cfg.oauth_base = format!("{}/oauth", self.mock.uri());
        cfg
    }

    async fn connect(&self, cfg: Config) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server = MoneyLoverServer::new(Arc::new(MoneyLover::new(cfg).unwrap()));
        tokio::spawn(async move {
            let running = server.serve(server_io).await.unwrap();
            let _ = running.waiting().await;
        });
        ().serve(client_io).await.unwrap()
    }

    async fn mount(&self, endpoint: &str, data: Value) {
        Mock::given(method("POST"))
            .and(path(format!("/api{endpoint}")))
            .and(header("authorization", "AuthJWT tok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"error": 0, "msg": "ok", "data": data})),
            )
            .mount(&self.mock)
            .await;
    }

    async fn mount_basics(&self) {
        self.mount(
            "/wallet/list",
            json!([
                {"_id": "w1", "name": "Cash", "balance": [{"IDR": "1250000.00"}]},
                {"_id": "w2", "name": "MCP Test", "balance": [{"IDR": "0"}]},
                {"_id": "w3", "name": "Old", "archived": true, "balance": [{"IDR": "5"}]},
            ]),
        )
        .await;
        // Like the real API: one row per (category, wallet), ids shared across wallets.
        let mut rows = Vec::new();
        for wallet in ["w1", "w2", "w3"] {
            rows.extend([
                json!({"_id": "c1", "name": "Food", "type": 2, "account": wallet}),
                json!({"_id": "c2", "name": "Lunch", "type": 2, "parent": "c1", "account": wallet}),
                json!({"_id": "c3", "name": "Salary", "type": 1, "account": wallet}),
                json!({"_id": "c4", "name": "Other", "type": 1, "account": wallet}),
                json!({"_id": "c5", "name": "Other", "type": 2, "account": wallet}),
            ]);
        }
        // A category that exists only in w1, and one hidden from w2.
        rows.push(json!({"_id": "c6", "name": "Cash only", "type": 2, "account": "w1"}));
        rows.push(json!({"_id": "c7", "name": "Hidden", "type": 2, "account": "w2", "exclude_accounts": ["w2"]}));
        self.mount("/category/list-all", Value::Array(rows)).await;
        // The ids from this endpoint must never be used.
        self.mount(
            "/category/list",
            json!([{"_id": "wrong", "name": "Food", "type": 2, "account": "w1"}]),
        )
        .await;
    }
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &str,
    args: Value,
) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_string())
        .with_arguments(args.as_object().cloned().unwrap_or_default());
    client.call_tool(params).await.unwrap()
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn structured(result: &CallToolResult) -> &Value {
    assert_ne!(
        result.is_error,
        Some(true),
        "tool failed: {}",
        text_of(result)
    );
    result
        .structured_content
        .as_ref()
        .expect("structured content")
}

#[tokio::test]
async fn add_transaction_stays_listed_but_refuses_when_read_only() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    // Any request at all would be a bug: a read-only start must never reach the API.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, false)).await;
    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(names.contains(&"list_wallets".to_string()));
    assert!(names.contains(&"summarize_spending".to_string()));
    // Listed, not hidden: a client (and a security scanner) sees the whole surface,
    // and the server, not the client, is what enforces read-only.
    for write_tool in [
        "add_transaction",
        "update_transaction",
        "delete_transaction",
    ] {
        assert!(names.contains(&write_tool.to_string()), "{names:?}");
    }

    let attempts = [
        (
            "add_transaction",
            json!({"wallet": "MCP Test", "category": "Food", "amount": 5, "dry_run": true}),
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": "2026-04-18", "amount": 5, "dry_run": true}),
        ),
        (
            "delete_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": "2026-04-18", "dry_run": true}),
        ),
    ];
    for (tool, args) in attempts {
        let result = call(&client, tool, args).await;
        assert_eq!(result.is_error, Some(true), "{tool}");
        assert!(
            text_of(&result).contains("MONEYLOVER_ALLOW_WRITE"),
            "{tool}: {}",
            text_of(&result)
        );
    }
}

#[tokio::test]
async fn write_flag_enables_add_transaction() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    let client = h.connect(h.config(true, false)).await;
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 7);
    assert!(tools.iter().any(|t| t.name == "add_transaction"));
    for name in ["update_transaction", "delete_transaction"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let annotations = tool.annotations.as_ref().unwrap();
        assert_eq!(annotations.read_only_hint, Some(false), "{name}");
        assert_eq!(annotations.destructive_hint, Some(true), "{name}");
    }
}

#[tokio::test]
async fn lists_wallets_with_balances() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    let wallets = structured(&result)["wallets"].as_array().unwrap().clone();
    assert_eq!(wallets.len(), 3);
    assert_eq!(wallets[0]["name"], "Cash");
    assert_eq!(wallets[0]["currency"], "IDR");
    assert_eq!(wallets[0]["balance"], "1250000");
}

#[tokio::test]
async fn lists_categories_for_wallet_with_hierarchy_and_type_filter() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "list_categories",
        json!({"wallet": "cash", "type": "expense"}),
    )
    .await;
    let names: Vec<String> = structured(&result)["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["full_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["Cash only", "Food", "Food > Lunch", "Other"]);

    let result = call(&client, "list_categories", json!({"wallet": "MCP Test"})).await;
    let out = structured(&result);
    assert_eq!(out["kind"], "both income and expense");
    assert_eq!(out["wallet"], "MCP Test");
    let names: Vec<String> = out["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["full_name"].as_str().unwrap().to_string())
        .collect();
    assert!(!names.contains(&"Cash only".to_string()), "{names:?}");
    assert!(!names.contains(&"Hidden".to_string()), "{names:?}");

    // Without a wallet, the per-wallet copies are collapsed.
    let result = call(&client, "list_categories", json!({"type": "income"})).await;
    let names: Vec<String> = structured(&result)["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["full_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["Other", "Salary"]);
    let out = structured(&result);
    assert_eq!(out["kind"], "income only");
    assert!(out["wallet"].is_null());
}

fn transactions_fixture() -> Value {
    json!({
        "daterange": {"startDate": "2026-04-01", "endDate": "2026-04-30"},
        "transactions": [
            {"_id": "t1", "amount": 50000, "note": "lunch", "displayDate": "2026-04-17T18:30:00.000Z",
             "account": {"_id": "w1", "name": "Cash"}, "category": {"_id": "c2", "name": "Lunch", "type": 2}},
            {"_id": "t2", "amount": "25000.50", "note": "coffee", "displayDate": "2026-04-20T03:00:00.000Z",
             "account": {"_id": "w1", "name": "Cash"}, "category": {"_id": "c2", "name": "Lunch", "type": 2}},
            {"_id": "t3", "amount": 9000000, "note": "", "displayDate": "2026-04-25T03:00:00.000Z",
             "account": {"_id": "w1", "name": "Cash"}, "category": {"_id": "c3", "name": "Salary", "type": 1}},
            {"_id": "t4", "amount": 100000, "note": "hidden", "displayDate": "2026-04-26T03:00:00.000Z", "exclude_report": true,
             "account": {"_id": "w1", "name": "Cash"}, "category": {"_id": "c2", "name": "Lunch", "type": 2}},
            {"_id": "t5", "amount": 1, "displayDate": "garbage",
             "account": {"_id": "w1", "name": "Cash"}, "category": {"_id": "c2", "name": "Lunch", "type": 2}}
        ]
    })
}

#[tokio::test]
async fn lists_transactions_newest_first_with_limit_and_timezone() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/list"))
        .and(body_json(
            json!({"walletId": "w1", "startDate": "2026-04-01", "endDate": "2026-04-30"}),
        ))
        .and(header("authorization", "AuthJWT tok"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": 0, "data": transactions_fixture()})),
        )
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "list_transactions",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30", "wallet": "Cash", "limit": 2}),
    )
    .await;
    let out = structured(&result);
    assert_eq!(out["total_count"], 4, "the unparsable row is skipped");
    assert_eq!(out["truncated"], true);
    let txs = out["transactions"].as_array().unwrap();
    assert_eq!(txs.len(), 2);
    assert_eq!(txs[0]["id"], "t4");
    assert_eq!(txs[1]["id"], "t3");
    assert_eq!(txs[1]["amount"], "9000000");
    assert_eq!(txs[1]["currency"], "IDR");

    // Category filter and timezone conversion (18:30Z on the 17th is the 18th in Jakarta).
    let result = call(
        &client,
        "list_transactions",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30", "wallet": "Cash", "category": "LUNCH", "limit": 200}),
    )
    .await;
    let out = structured(&result);
    assert_eq!(out["total_count"], 3);
    let oldest = out["transactions"].as_array().unwrap().last().unwrap();
    assert_eq!(oldest["id"], "t1");
    assert_eq!(oldest["date"], "2026-04-18");
}

#[tokio::test]
async fn summarizes_spending_per_category_and_skips_excluded() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/list"))
        .and(body_json(
            json!({"walletId": "all", "startDate": "2026-04-01", "endDate": "2026-04-30"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": 0, "data": transactions_fixture()})),
        )
        .mount(&h.mock)
        .await;
    // "all" may leave out the archived wallet, so it is asked separately.
    mount_wallet_transactions(
        &h,
        "w3",
        json!([{"_id": "t-old", "amount": 1000, "displayDate": "2026-04-02T03:00:00.000Z",
                "account": {"_id": "w3", "name": "Old"},
                "category": {"_id": "c1", "name": "Food", "type": 2}}]),
    )
    .await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "summarize_spending",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30"}),
    )
    .await;
    let out = structured(&result);
    assert_eq!(out["type"], "expense");
    let idr = &out["currencies"][0];
    assert_eq!(idr["currency"], "IDR");
    assert_eq!(idr["total"], "76000.5", "t4 is excluded from reports");
    assert_eq!(idr["count"], 3);
    assert_eq!(idr["categories"][0]["category"], "Lunch");
    assert_eq!(idr["categories"][1]["category"], "Food");
}

/// Answers `/transaction/list` for one wallet in April 2026.
async fn mount_wallet_transactions(h: &Harness, wallet_id: &str, transactions: Value) {
    Mock::given(method("POST"))
        .and(path("/api/transaction/list"))
        .and(body_json(
            json!({"walletId": wallet_id, "startDate": "2026-04-01", "endDate": "2026-04-30"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": 0, "data": {"transactions": transactions}})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
}

fn april_row(id: &str, wallet: (&str, &str)) -> Value {
    json!({"_id": id, "amount": 10, "note": "", "displayDate": "2026-04-10T03:00:00.000Z",
           "account": {"_id": wallet.0, "name": wallet.1},
           "category": {"_id": "c1", "name": "Food", "type": 2}})
}

fn ids_listed(result: &CallToolResult) -> Vec<String> {
    let mut ids: Vec<String> = structured(result)["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn all_wallets_includes_wallets_excluded_from_the_total_once() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount(
        "/wallet/list",
        json!([
            {"_id": "w1", "name": "Cash", "balance": [{"IDR": "1"}]},
            {"_id": "w2", "name": "Savings", "exclude_total": true, "balance": [{"IDR": "1"}]},
        ]),
    )
    .await;
    // Like the real API, "all" leaves out the wallet excluded from the total; a row
    // that shows up in both answers is listed once.
    mount_wallet_transactions(
        &h,
        "all",
        json!([
            april_row("a", ("w1", "Cash")),
            april_row("both", ("w2", "Savings"))
        ]),
    )
    .await;
    mount_wallet_transactions(
        &h,
        "w2",
        json!([
            april_row("b", ("w2", "Savings")),
            april_row("both", ("w2", "Savings"))
        ]),
    )
    .await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "list_transactions",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30"}),
    )
    .await;
    assert_eq!(ids_listed(&result), ["a", "b", "both"]);
}

#[tokio::test]
async fn a_full_page_from_all_falls_back_to_every_wallet() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount(
        "/wallet/list",
        json!([
            {"_id": "w1", "name": "Cash", "balance": [{"IDR": "1"}]},
            {"_id": "w2", "name": "Bank", "balance": [{"IDR": "1"}]},
        ]),
    )
    .await;
    // 9999 rows is Money Lover's cap, so "all" is incomplete and its rows unused.
    let capped: Vec<Value> = (0..9999)
        .map(|i| april_row(&format!("capped{i}"), ("w1", "Cash")))
        .collect();
    mount_wallet_transactions(&h, "all", Value::Array(capped)).await;
    mount_wallet_transactions(&h, "w1", json!([april_row("a", ("w1", "Cash"))])).await;
    mount_wallet_transactions(&h, "w2", json!([april_row("b", ("w2", "Bank"))])).await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "list_transactions",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30"}),
    )
    .await;
    assert_eq!(ids_listed(&result), ["a", "b"]);
}

#[tokio::test]
async fn a_full_page_from_one_wallet_is_an_error_not_a_wrong_total() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount(
        "/wallet/list",
        json!([{"_id": "w1", "name": "Cash", "balance": [{"IDR": "1"}]}]),
    )
    .await;
    let capped: Vec<Value> = (0..9999)
        .map(|i| april_row(&format!("capped{i}"), ("w1", "Cash")))
        .collect();
    mount_wallet_transactions(&h, "w1", Value::Array(capped)).await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(
        &client,
        "summarize_spending",
        json!({"start_date": "2026-04-01", "end_date": "2026-04-30", "wallet": "Cash"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        text_of(&result).contains("shorter date ranges"),
        "{}",
        text_of(&result)
    );
}

#[tokio::test]
async fn add_transaction_dry_run_never_writes() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": {"_id": "new"}})),
        )
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(true, false)).await;

    let result = call(
        &client,
        "add_transaction",
        json!({"wallet": "mcp test", "category": "Food > Lunch", "amount": "50000", "date": recent_date(), "note": "nasi padang", "dry_run": true}),
    )
    .await;
    let out = structured(&result);
    assert_eq!(out["created"], false);
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["wallet"], "MCP Test");
    assert_eq!(out["category"], "Food > Lunch (expense)");
    assert_eq!(out["currency"], "IDR");
    // The `expect(0)` above is verified when the mock server is dropped.
}

#[tokio::test]
async fn add_transaction_sends_resolved_ids_and_amount() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .and(header("authorization", "AuthJWT tok"))
        .and(body_json(json!({
            "with": [], "account": "w2", "category": "c2",
            "amount": "50000", "note": "nasi padang", "displayDate": recent_date()
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": 0, "data": {"_id": "web123"}})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(true, false)).await;

    let result = call(
        &client,
        "add_transaction",
        json!({"wallet": "MCP Test", "category": "lunch", "amount": 50000, "date": recent_date(), "note": "nasi padang"}),
    )
    .await;
    let out = structured(&result);
    assert_eq!(out["created"], true);
    assert_eq!(out["id"], "web123");
    assert_eq!(out["type"], "expense");
}

#[tokio::test]
async fn add_transaction_rejects_bad_input_before_any_write() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": {}})))
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(true, false)).await;

    let cases = [
        (
            json!({"wallet": "MCP Test", "category": "Food", "amount": -5}),
            "greater than zero",
        ),
        (
            json!({"wallet": "MCP Test", "category": "Food", "amount": "abc"}),
            "not a valid number",
        ),
        (
            json!({"wallet": "Nope", "category": "Food", "amount": 5}),
            "Available wallets",
        ),
        (
            json!({"wallet": "Old", "category": "Food", "amount": 5}),
            "archived",
        ),
        (
            json!({"wallet": "MCP Test", "category": "Other", "amount": 5}),
            "ambiguous",
        ),
        (
            json!({"wallet": "MCP Test", "category": "Nonexistent", "amount": 5}),
            "not found",
        ),
        (
            json!({"wallet": "MCP Test", "category": "Food", "amount": 5, "date": "yesterday"}),
            "YYYY-MM-DD",
        ),
    ];
    for (args, expected) in cases {
        let result = call(&client, "add_transaction", args.clone()).await;
        assert_eq!(result.is_error, Some(true), "{args}");
        assert!(
            text_of(&result).contains(expected),
            "{args}: {}",
            text_of(&result)
        );
    }
    // A type resolves the ambiguity.
    let ok = call(
        &client,
        "add_transaction",
        json!({"wallet": "MCP Test", "category": "Other", "amount": 5, "type": "income", "dry_run": true}),
    )
    .await;
    assert_eq!(structured(&ok)["category"], "Other (income)");
}

#[tokio::test]
async fn logs_in_once_and_persists_the_tokens() {
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": 0,
            "data": {"status": true, "request_token": "req-tok",
                     "login_url": "https://web.moneylover.me/login?client=client-xyz&token=req-tok"}
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("authorization", "Bearer req-tok"))
        .and(header("client", "client-xyz"))
        .and(body_string_contains("email=me%40example.com"))
        .and(body_string_contains("password=hunter2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": true, "access_token": "tok", "refresh_token": "ref1", "expire": FAR_FUTURE
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    h.mount_basics().await;
    let client = h.connect(h.config(false, true)).await;

    // Two calls, but only one login.
    call(&client, "list_wallets", json!({})).await;
    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&result)["wallets"].as_array().unwrap().len(), 3);

    let stored = load_tokens(&h.token_path()).unwrap().unwrap();
    assert_eq!(stored.access_token, "tok");
    assert_eq!(stored.refresh_token.as_deref(), Some("ref1"));
}

#[tokio::test]
async fn refreshes_a_token_that_is_about_to_expire_without_logging_in() {
    let h = Harness::new().await;
    let soon = chrono::Utc::now().timestamp() + 3600;
    h.seed_token("old", "ref-old", soon);
    Mock::given(method("POST"))
        .and(path("/api/user/refresh-token"))
        .and(body_json(json!({"refreshToken": "ref-old"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": 0, "msg": "refresh_token_success",
            "data": {"status": true, "access_token": "tok", "refresh_token": "ref-new", "expire": FAR_FUTURE}
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    h.mount_basics().await;
    let client = h.connect(h.config(false, true)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&result)["wallets"].as_array().unwrap().len(), 3);
    let stored = load_tokens(&h.token_path()).unwrap().unwrap();
    assert_eq!(stored.access_token, "tok");
    assert_eq!(stored.refresh_token.as_deref(), Some("ref-new"));
}

#[tokio::test]
async fn a_rejected_token_triggers_one_refresh_then_succeeds() {
    let h = Harness::new().await;
    h.seed_token("stale", "ref", FAR_FUTURE);
    // The stale token is rejected with the e=706 shape (HTTP 200).
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .and(header("authorization", "AuthJWT stale"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"s": false, "e": 706, "message": "token rejected"})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/user/refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": 0, "data": {"access_token": "tok", "refresh_token": "ref2", "expire": FAR_FUTURE}
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    h.mount_basics().await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&result)["wallets"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn api_errors_become_tool_errors_not_crashes() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": 5, "msg": "something broke"})),
        )
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, false)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(text_of(&result).contains("something broke"));
}

#[tokio::test]
async fn reads_are_retried_on_server_errors_but_writes_are_not() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    // First two wallet/list calls fail with 503, the third works.
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.mock)
        .await;
    h.mount_basics().await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(true, false)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&result)["wallets"].as_array().unwrap().len(), 3);

    let result = call(
        &client,
        "add_transaction",
        json!({"wallet": "MCP Test", "category": "Food", "amount": 5}),
    )
    .await;
    assert_eq!(
        result.is_error,
        Some(true),
        "a failed write is reported, never retried"
    );
}

async fn mount_categories_only(h: &Harness) {
    let rows: Vec<Value> = ["w1", "w2"]
        .iter()
        .map(|w| json!({"_id": "c1", "name": "Food", "type": 2, "account": w}))
        .collect();
    h.mount("/category/list-all", Value::Array(rows)).await;
}

fn wallets_body(names: &[(&str, &str)]) -> Value {
    let wallets: Vec<Value> = names
        .iter()
        .map(|(id, name)| json!({"_id": id, "name": name, "balance": [{"IDR": "10"}]}))
        .collect();
    json!({"error": 0, "data": wallets})
}

#[tokio::test]
async fn wallets_and_categories_are_cached_across_tools() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wallets_body(&[("w1", "Cash")])))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/category/list-all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": 0, "data": [{"_id": "c1", "name": "Food", "type": 2, "account": "w1"}]
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(true, false)).await;

    let first = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&first)["from_cache"], false);
    let second = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&second)["from_cache"], true);
    for _ in 0..2 {
        let r = call(&client, "list_categories", json!({"wallet": "Cash"})).await;
        structured(&r);
        let r = call(
            &client,
            "add_transaction",
            json!({"wallet": "Cash", "category": "Food", "amount": 1, "dry_run": true}),
        )
        .await;
        structured(&r);
    }
    // `expect(1)` on both mocks is verified when the mock server is dropped.
}

#[tokio::test]
async fn refresh_bypasses_the_cache() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wallets_body(&[("w1", "Cash")])))
        .expect(2)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, false)).await;

    call(&client, "list_wallets", json!({})).await;
    let r = call(&client, "list_wallets", json!({"refresh": true})).await;
    assert_eq!(structured(&r)["from_cache"], false);
    let r = call(&client, "list_wallets", json!({})).await;
    assert_eq!(
        structured(&r)["from_cache"],
        true,
        "refresh repopulates the cache"
    );
}

#[tokio::test]
async fn a_write_invalidates_cached_balances() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wallets_body(&[("w1", "Cash")])))
        .expect(2)
        .mount(&h.mock)
        .await;
    mount_categories_only(&h).await;
    h.mount("/transaction/add", json!({"_id": "t-new"})).await;
    let client = h.connect(h.config(true, false)).await;

    call(&client, "list_wallets", json!({})).await;
    let r = call(
        &client,
        "add_transaction",
        json!({"wallet": "Cash", "category": "Food", "amount": 1}),
    )
    .await;
    assert_eq!(structured(&r)["created"], true);
    let r = call(&client, "list_wallets", json!({})).await;
    assert_eq!(
        structured(&r)["from_cache"],
        false,
        "balances are re-fetched after a write"
    );
}

#[tokio::test]
async fn a_wallet_missing_from_the_cache_triggers_one_fresh_fetch() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    // First answer predates the new wallet; later answers include it.
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wallets_body(&[("w1", "Cash")])))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(wallets_body(&[("w1", "Cash"), ("w2", "Brand New")])),
        )
        // One fresh fetch for "brand new", one for "Ghost": each miss refetches exactly once.
        .expect(2)
        .mount(&h.mock)
        .await;
    mount_categories_only(&h).await;
    let client = h.connect(h.config(false, false)).await;

    call(&client, "list_wallets", json!({})).await;
    let r = call(&client, "list_categories", json!({"wallet": "brand new"})).await;
    assert_eq!(structured(&r)["categories"][0]["name"], "Food");

    // A name that really does not exist is still an error (after one fresh fetch, not a loop).
    let r = call(&client, "list_categories", json!({"wallet": "Ghost"})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("Brand New"), "{}", text_of(&r));
}

// ---------------------------------------------------------------------------
// Write safeguards
// ---------------------------------------------------------------------------

async fn write_harness(
    configure: impl FnOnce(&mut Config),
) -> (Harness, rmcp::service::RunningService<rmcp::RoleClient, ()>) {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    let mut cfg = h.config(true, false);
    configure(&mut cfg);
    let client = h.connect(cfg).await;
    (h, client)
}

fn lunch(amount: i64) -> Value {
    json!({"wallet": "MCP Test", "category": "Lunch", "amount": amount, "date": recent_date()})
}

#[tokio::test]
async fn an_identical_write_is_refused_unless_explicitly_allowed() {
    let (h, client) = write_harness(|_| {}).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": {"_id": "t"}})),
        )
        .expect(2)
        .mount(&h.mock)
        .await;

    assert_eq!(
        structured(&call(&client, "add_transaction", lunch(5)).await)["created"],
        true
    );
    let again = call(&client, "add_transaction", lunch(5)).await;
    assert_eq!(again.is_error, Some(true));
    assert!(
        text_of(&again).contains("allow_duplicate"),
        "{}",
        text_of(&again)
    );

    let mut explicit = lunch(5);
    explicit["allow_duplicate"] = json!(true);
    assert_eq!(
        structured(&call(&client, "add_transaction", explicit).await)["created"],
        true
    );
}

#[tokio::test]
async fn a_timed_out_write_is_reported_as_uncertain_and_blocks_a_blind_retry() {
    let (h, client) = write_harness(|_| {}).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&h.mock)
        .await;

    let first = call(&client, "add_transaction", lunch(7)).await;
    assert_eq!(first.is_error, Some(true));
    assert!(
        text_of(&first).starts_with("UNCERTAIN"),
        "{}",
        text_of(&first)
    );
    let retry = call(&client, "add_transaction", lunch(7)).await;
    assert!(
        text_of(&retry).contains("allow_duplicate"),
        "{}",
        text_of(&retry)
    );
}

#[tokio::test]
async fn hourly_write_limit_is_enforced() {
    let (h, client) = write_harness(|cfg| cfg.max_writes_per_hour = 2).await;
    h.mount("/transaction/add", json!({"_id": "t"})).await;

    for amount in [1, 2] {
        structured(&call(&client, "add_transaction", lunch(amount)).await);
    }
    let third = call(&client, "add_transaction", lunch(3)).await;
    assert!(
        text_of(&third).contains("write limit"),
        "{}",
        text_of(&third)
    );
    // Dry runs never count and still work.
    let mut preview = lunch(4);
    preview["dry_run"] = json!(true);
    structured(&call(&client, "add_transaction", preview).await);
}

#[tokio::test]
async fn configured_wallet_allowlist_amount_cap_and_date_window_are_enforced() {
    let (h, client) = write_harness(|cfg| {
        cfg.write_wallets = Some(vec!["mcp test".into()]);
        cfg.max_amount = Some(rust_decimal::Decimal::from(100));
    })
    .await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": {}})))
        .expect(0)
        .mount(&h.mock)
        .await;

    let cases = [
        (
            json!({"wallet": "Cash", "category": "Lunch", "amount": 5}),
            "MONEYLOVER_WRITE_WALLETS",
        ),
        (lunch(101), "MONEYLOVER_MAX_AMOUNT"),
        (
            json!({"wallet": "MCP Test", "category": "Lunch", "amount": 5, "date": "2001-01-01"}),
            "outside the allowed window",
        ),
    ];
    for (args, expected) in cases {
        let result = call(&client, "add_transaction", args.clone()).await;
        assert!(
            text_of(&result).contains(expected),
            "{args}: {}",
            text_of(&result)
        );
    }
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/elsewhere", h.mock.uri())),
        )
        .mount(&h.mock)
        .await;
    Mock::given(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": []})))
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, false)).await;
    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn a_wrong_password_is_not_retried_on_every_call() {
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "error": 0,
            "data": {"request_token": "req", "login_url": "https://web.moneylover.me/login?client=c"}
        })))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": false, "message": "wrong password"})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, true)).await;

    for _ in 0..3 {
        let result = call(&client, "list_wallets", json!({})).await;
        assert!(
            text_of(&result).contains("wrong password"),
            "{}",
            text_of(&result)
        );
    }
}

#[tokio::test]
async fn a_removed_device_does_not_silently_log_in_again() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"s": false, "e": 717, "message": "device removed"})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, true)).await;

    for _ in 0..2 {
        let result = call(&client, "list_wallets", json!({})).await;
        assert!(
            text_of(&result).contains("moneylover-mcp login"),
            "{}",
            text_of(&result)
        );
    }
    assert!(
        load_tokens(&h.token_path()).unwrap().is_none(),
        "the stored token is deleted"
    );
}

#[tokio::test]
async fn a_token_refreshed_by_another_process_is_picked_up_instead_of_logging_in() {
    let h = Harness::new().await;
    h.seed_token("stale", "ref-old", FAR_FUTURE);
    // This process gets "token rejected" for the stale token...
    Mock::given(method("POST"))
        .and(path("/api/wallet/list"))
        .and(header("authorization", "AuthJWT stale"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"s": false, "e": 706})))
        .mount(&h.mock)
        .await;
    // ...and must neither refresh with the old refresh token nor log in.
    Mock::given(method("POST"))
        .and(path("/api/user/refresh-token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    h.mount_basics().await;
    let client = h.connect(h.config(false, true)).await;

    // Another process already rotated the tokens on disk.
    h.seed_token("tok", "ref-new", FAR_FUTURE);
    let result = call(&client, "list_wallets", json!({})).await;
    assert_eq!(structured(&result)["wallets"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn logout_stops_a_running_server_immediately() {
    let h = Harness::new().await;
    h.seed_token("tok", "ref", FAR_FUTURE);
    h.mount_basics().await;
    let client = h.connect(h.config(false, true)).await;
    structured(&call(&client, "list_wallets", json!({"refresh": true})).await);

    // What `moneylover-mcp logout` does, from another process.
    moneylover_mcp::token_store::mark_revoked(&h.token_path());
    moneylover_mcp::token_store::delete_tokens(&h.token_path());

    let result = call(&client, "list_wallets", json!({"refresh": true})).await;
    assert!(
        text_of(&result).contains("signed out"),
        "{}",
        text_of(&result)
    );
    assert!(
        load_tokens(&h.token_path()).unwrap().is_none(),
        "the token file is not written back"
    );
}

#[tokio::test]
async fn a_device_removed_during_refresh_signs_out_instead_of_logging_in() {
    let h = Harness::new().await;
    h.seed_token("old", "ref", chrono::Utc::now().timestamp() + 60);
    Mock::given(method("POST"))
        .and(path("/api/user/refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"s": false, "e": 717})))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/user/login-url"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.mock)
        .await;
    let client = h.connect(h.config(false, true)).await;

    let result = call(&client, "list_wallets", json!({})).await;
    assert!(
        text_of(&result).contains("signed out"),
        "{}",
        text_of(&result)
    );
}

// ---------------------------------------------------------------------------
// Updating and deleting existing transactions
// ---------------------------------------------------------------------------

fn days_ago(days: u64) -> chrono::NaiveDate {
    chrono::Utc::now().date_naive() - chrono::Days::new(days)
}

/// A plain transaction and half of a transfer in "MCP Test", one transaction in
/// "Cash", all a few days old. Each wallet answers only the request that looks it
/// up (its own id, the stored date with one day either side), like the real API
/// where `walletId: "all"` leaves out wallets excluded from the total.
async fn mount_existing_transactions(h: &Harness) -> String {
    let date = days_ago(3);
    let display = format!("{date}T05:00:00.000Z");
    let per_wallet = [
        (
            "w2",
            json!([
                {"_id": "t1", "amount": 50000, "note": "nasi padang", "displayDate": display,
                 "account": {"_id": "w2", "name": "MCP Test"},
                 "category": {"_id": "c2", "name": "Lunch", "type": 2},
                 "with": ["Ann"], "exclude_report": true, "images": ["receipt.jpg"]},
                {"_id": "t-transfer", "amount": 10, "note": "", "displayDate": display, "related": "t-other",
                 "account": {"_id": "w2", "name": "MCP Test"},
                 "category": {"_id": "c5", "name": "Other", "type": 2}}
            ]),
        ),
        (
            "w1",
            json!([
                {"_id": "t-cash", "amount": 10, "note": "", "displayDate": display,
                 "account": {"_id": "w1", "name": "Cash"},
                 "category": {"_id": "c1", "name": "Food", "type": 2}}
            ]),
        ),
    ];
    for (wallet_id, transactions) in per_wallet {
        Mock::given(method("POST"))
            .and(path("/api/transaction/list"))
            .and(body_json(json!({
                "walletId": wallet_id,
                "startDate": (date - chrono::Days::new(1)).to_string(),
                "endDate": (date + chrono::Days::new(1)).to_string(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": 0,
                "data": {"transactions": transactions}
            })))
            .mount(&h.mock)
            .await;
    }
    date.to_string()
}

#[tokio::test]
async fn update_transaction_changes_only_what_was_asked_and_keeps_the_rest() {
    let (h, client) = write_harness(|_| {}).await;
    let date = mount_existing_transactions(&h).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/edit"))
        .and(header("authorization", "AuthJWT tok"))
        .and(body_json(json!({
            "_id": "t1", "with": ["Ann"], "account": "w2", "category": "c1",
            "amount": "45000", "note": "nasi padang", "displayDate": date, "event": "",
            "exclude_report": true, "longtitude": 0, "latitude": 0,
            "addressName": "", "addressDetails": "", "addressIcon": "", "image": "receipt.jpg"
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"error": 0, "msg": "transaction_edit_success", "data": {}})),
        )
        .expect(1)
        .mount(&h.mock)
        .await;

    let args = json!({"id": "t1", "wallet": "MCP Test", "date": date, "category": "Food", "amount": 45000});
    let mut preview = args.clone();
    preview["dry_run"] = json!(true);
    let out = structured(&call(&client, "update_transaction", preview).await).clone();
    assert_eq!(out["updated"], false);
    assert_eq!(out["before"]["category"], "Lunch");
    assert_eq!(out["after"]["category"], "Food");

    let out = structured(&call(&client, "update_transaction", args).await).clone();
    assert_eq!(out["updated"], true);
    assert_eq!(out["before"]["amount"], "50000");
    assert_eq!(out["after"]["amount"], "45000");
    assert_eq!(out["after"]["note"], "nasi padang");
    assert_eq!(out["after"]["wallet"], "MCP Test");
    // `expect(1)`: the dry run sent nothing.
}

#[tokio::test]
async fn update_transaction_can_move_the_date_and_clear_the_note() {
    let (h, client) = write_harness(|_| {}).await;
    let date = mount_existing_transactions(&h).await;
    let new_date = days_ago(1).to_string();
    Mock::given(method("POST"))
        .and(path("/api/transaction/edit"))
        .and(body_string_contains(format!(
            "\"displayDate\":\"{new_date}\""
        )))
        .and(body_string_contains("\"note\":\"\""))
        .and(body_string_contains("\"category\":\"c2\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0, "data": {}})))
        .expect(1)
        .mount(&h.mock)
        .await;

    let out = call(
        &client,
        "update_transaction",
        json!({"id": "t1", "wallet": "MCP Test", "date": date, "new_date": new_date, "note": "  "}),
    )
    .await;
    assert_eq!(structured(&out)["after"]["date"], new_date);
}

#[tokio::test]
async fn delete_transaction_sends_only_the_id_and_supports_dry_run() {
    let (h, client) = write_harness(|_| {}).await;
    let date = mount_existing_transactions(&h).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/delete"))
        .and(header("authorization", "AuthJWT tok"))
        .and(body_json(json!({"_id": "t1", "delRelated": false})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"error": 0, "msg": "transaction_delete_success", "data": {}}),
            ),
        )
        .expect(1)
        .mount(&h.mock)
        .await;

    let preview = call(
        &client,
        "delete_transaction",
        json!({"id": "t1", "wallet": "MCP Test", "date": date, "dry_run": true}),
    )
    .await;
    let out = structured(&preview);
    assert_eq!(out["deleted"], false);
    assert_eq!(out["transaction"]["note"], "nasi padang");

    let done = call(
        &client,
        "delete_transaction",
        json!({"id": "t1", "wallet": "MCP Test", "date": date}),
    )
    .await;
    assert_eq!(structured(&done)["deleted"], true);
}

#[tokio::test]
async fn update_and_delete_refuse_before_any_write() {
    let (h, client) = write_harness(|cfg| {
        cfg.write_wallets = Some(vec!["mcp test".into()]);
        cfg.max_amount = Some(rust_decimal::Decimal::from(100_000));
    })
    .await;
    let date = mount_existing_transactions(&h).await;
    for endpoint in ["/api/transaction/edit", "/api/transaction/delete"] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0})))
            .expect(0)
            .mount(&h.mock)
            .await;
    }

    let cases = [
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date}),
            "nothing to change",
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "type": "income"}),
            "nothing to change",
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "note": "x", "type": "income"}),
            "only used together with category",
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "amount": 100_001}),
            "MONEYLOVER_MAX_AMOUNT",
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "new_date": "2001-01-01"}),
            "outside the allowed window",
        ),
        (
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "category": "Cash only"}),
            "not found",
        ),
        (
            "update_transaction",
            json!({"id": "t-cash", "wallet": "Cash", "date": date, "note": "x"}),
            "MONEYLOVER_WRITE_WALLETS",
        ),
        (
            "delete_transaction",
            json!({"id": "t-cash", "wallet": "Cash", "date": date}),
            "MONEYLOVER_WRITE_WALLETS",
        ),
        (
            "delete_transaction",
            json!({"id": "t-transfer", "wallet": "MCP Test", "date": date}),
            "transfer, debt or loan",
        ),
        (
            "delete_transaction",
            json!({"id": "missing", "wallet": "MCP Test", "date": date}),
            "not found in wallet 'MCP Test'",
        ),
        (
            "delete_transaction",
            json!({"id": "t1", "wallet": "Cash", "date": date}),
            "not found in wallet 'Cash'",
        ),
        (
            "delete_transaction",
            json!({"id": "t1", "wallet": "Nope", "date": date}),
            "Available wallets",
        ),
        (
            "delete_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": "18/04/2026"}),
            "YYYY-MM-DD",
        ),
    ];
    for (tool, args, expected) in cases {
        let result = call(&client, tool, args.clone()).await;
        assert_eq!(result.is_error, Some(true), "{tool} {args}");
        assert!(
            text_of(&result).contains(expected),
            "{tool} {args}: {}",
            text_of(&result)
        );
    }
}

#[tokio::test]
async fn transactions_outside_the_write_window_cannot_be_changed() {
    let (h, client) = write_harness(|_| {}).await;
    h.mount(
        "/transaction/list",
        json!([{"_id": "old", "amount": 1, "displayDate": "2001-01-01",
                "account": {"_id": "w2", "name": "MCP Test"},
                "category": {"_id": "c1", "name": "Food", "type": 2}}]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": 0})))
        .expect(0)
        .mount(&h.mock)
        .await;

    let result = call(
        &client,
        "delete_transaction",
        json!({"id": "old", "wallet": "MCP Test", "date": "2001-01-01"}),
    )
    .await;
    assert!(
        text_of(&result).contains("outside the allowed window"),
        "{}",
        text_of(&result)
    );
}

#[tokio::test]
async fn a_timed_out_delete_is_reported_as_uncertain_and_counts_towards_the_limit() {
    let (h, client) = write_harness(|cfg| cfg.max_writes_per_hour = 2).await;
    let date = mount_existing_transactions(&h).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/delete"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&h.mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/edit"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": 7, "msg": "rejected"})),
        )
        .expect(2)
        .mount(&h.mock)
        .await;

    let first = call(
        &client,
        "delete_transaction",
        json!({"id": "t1", "wallet": "MCP Test", "date": date}),
    )
    .await;
    assert!(
        text_of(&first).starts_with("UNCERTAIN"),
        "{}",
        text_of(&first)
    );
    // A rejected edit certainly changed nothing, so it does not use up the second slot.
    for _ in 0..2 {
        let rejected = call(
            &client,
            "update_transaction",
            json!({"id": "t1", "wallet": "MCP Test", "date": date, "note": "x"}),
        )
        .await;
        assert!(
            text_of(&rejected).contains("rejected"),
            "{}",
            text_of(&rejected)
        );
    }
}

#[tokio::test]
async fn a_write_the_server_rejected_can_be_retried_without_allow_duplicate() {
    let (h, client) = write_harness(|_| {}).await;
    Mock::given(method("POST"))
        .and(path("/api/transaction/add"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": 9, "msg": "try later"})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.mock)
        .await;
    h.mount("/transaction/add", json!({"_id": "t"})).await;

    let first = call(&client, "add_transaction", lunch(9)).await;
    assert!(text_of(&first).contains("try later"), "{}", text_of(&first));
    assert!(!text_of(&first).contains("UNCERTAIN"));
    let retry = call(&client, "add_transaction", lunch(9)).await;
    assert_eq!(structured(&retry)["created"], true);
}
